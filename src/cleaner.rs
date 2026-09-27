//! Scanning a chat for matching messages and deleting them.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::client::TdError;
use crate::matcher::Matcher;
use crate::message::{Deletability, MessageInfo};
use crate::signal;
use crate::telegram::{Telegram, PAGE_SIZE};

const MAX_FLOOD_WAITS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScanMode {
    /// Read the whole chat history and match locally (exact, slower for huge chats)
    Full,
    /// Use Telegram's server-side search per keyword, then match locally
    /// (faster, but the server may miss matches inside words)
    Search,
}

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub mode: ScanMode,
    /// Stop after examining this many messages.
    pub limit: Option<usize>,
}

/// Receives progress notifications from long-running operations.
pub trait Progress {
    fn scanned(&self, _scanned: usize, _matched: usize) {}
    fn deleted(&self, _done: usize, _total: usize) {}
    fn waiting(&self, _seconds: u64) {}
}

#[cfg(test)]
pub struct NoProgress;

#[cfg(test)]
impl Progress for NoProgress {}

#[derive(Debug, Clone)]
pub struct Match {
    pub message: MessageInfo,
    pub keywords: Vec<String>,
    /// `None` when TDLib could not tell.
    pub deletability: Option<Deletability>,
}

#[derive(Debug)]
pub struct ScanOutcome {
    pub scanned: usize,
    /// Newest first.
    pub matches: Vec<Match>,
    /// The user pressed Ctrl-C; the results are partial.
    pub interrupted: bool,
}

struct Scan<'a> {
    matcher: &'a Matcher,
    limit: usize,
    seen: HashSet<i64>,
    scanned: usize,
    matches: Vec<Match>,
}

impl Scan<'_> {
    /// Returns false once the scan limit is reached.
    fn consider(&mut self, message: MessageInfo) -> bool {
        if !self.seen.insert(message.id) {
            return true;
        }
        self.scanned += 1;
        if let Some(found) = self.matcher.find(&message.text) {
            let keywords = found.into_iter().map(str::to_owned).collect();
            let deletability = message.deletability;
            self.matches.push(Match {
                message,
                keywords,
                deletability,
            });
        }
        self.scanned < self.limit
    }
}

enum Stop {
    Done,
    Interrupted,
}

pub fn scan(
    tg: &Telegram,
    chat_id: i64,
    matcher: &Matcher,
    options: &ScanOptions,
    progress: &dyn Progress,
) -> Result<ScanOutcome> {
    if options.mode == ScanMode::Search && matcher.options().regex {
        bail!("server search only works with plain keywords; use the full scan for regular expressions");
    }
    let mut scan = Scan {
        matcher,
        limit: options.limit.unwrap_or(usize::MAX),
        seen: HashSet::new(),
        scanned: 0,
        matches: Vec::new(),
    };
    let stop = match options.mode {
        ScanMode::Full => scan_history(tg, chat_id, &mut scan, progress)?,
        ScanMode::Search => scan_search(tg, chat_id, &mut scan, progress)?,
    };

    let mut matches = scan.matches;
    matches.sort_by_key(|m| Reverse(m.message.id));
    for m in matches.iter_mut().filter(|m| m.deletability.is_none()) {
        m.deletability = tg.deletability(chat_id, m.message.id).ok();
    }
    Ok(ScanOutcome {
        scanned: scan.scanned,
        matches,
        interrupted: matches!(stop, Stop::Interrupted),
    })
}

fn scan_history(tg: &Telegram, chat_id: i64, scan: &mut Scan, progress: &dyn Progress) -> Result<Stop> {
    let mut from_message_id = 0;
    loop {
        if signal::cancelled() {
            return Ok(Stop::Interrupted);
        }
        let page = match with_flood_wait(progress, || tg.history(chat_id, from_message_id)) {
            Ok(page) => page,
            Err(_) if signal::cancelled() => return Ok(Stop::Interrupted),
            Err(err) => return Err(err.into()),
        };
        let messages: Vec<MessageInfo> = page.iter().filter_map(MessageInfo::from_json).collect();
        let Some(oldest) = messages.iter().map(|m| m.id).min() else {
            return Ok(Stop::Done);
        };
        let seen_before = scan.seen.len();
        for message in messages {
            if !scan.consider(message) {
                return Ok(Stop::Done);
            }
        }
        progress.scanned(scan.scanned, scan.matches.len());
        // No new messages means TDLib has nothing older to give us.
        if scan.seen.len() == seen_before {
            return Ok(Stop::Done);
        }
        from_message_id = oldest;
    }
}

fn scan_search(tg: &Telegram, chat_id: i64, scan: &mut Scan, progress: &dyn Progress) -> Result<Stop> {
    let keywords = scan.matcher.keywords().to_vec();
    // With "match all", every result must contain the first keyword anyway.
    let queries = if scan.matcher.options().require_all {
        &keywords[..1]
    } else {
        &keywords[..]
    };
    for query in queries {
        let mut from_message_id = 0;
        loop {
            if signal::cancelled() {
                return Ok(Stop::Interrupted);
            }
            let (page, next) = match with_flood_wait(progress, || tg.search(chat_id, query, from_message_id)) {
                Ok(result) => result,
                Err(_) if signal::cancelled() => return Ok(Stop::Interrupted),
                Err(err) => return Err(err.into()),
            };
            let empty = page.is_empty();
            for message in page.iter().filter_map(MessageInfo::from_json) {
                if !scan.consider(message) {
                    return Ok(Stop::Done);
                }
            }
            progress.scanned(scan.scanned, scan.matches.len());
            if empty || next == 0 || next == from_message_id {
                break;
            }
            from_message_id = next;
        }
    }
    Ok(Stop::Done)
}

/// Which matched messages to delete, and how.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeletionPlan {
    /// Deleted with `revoke = true`.
    pub for_everyone: Vec<i64>,
    /// Deleted with `revoke = false`: removed from your view only.
    pub for_self: Vec<i64>,
    /// Messages that will be left alone, with the reason.
    pub skipped: Vec<(i64, &'static str)>,
}

impl DeletionPlan {
    pub fn len(&self) -> usize {
        self.for_everyone.len() + self.for_self.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Decides how each match gets deleted. With `revoke` the messages are deleted for
/// everyone where allowed, falling back to deleting them just for yourself. Without
/// it only messages that can be hidden for yourself alone are touched, so nothing
/// is ever removed for other people by accident.
pub fn plan_deletion(matches: &[Match], revoke: bool) -> DeletionPlan {
    let mut plan = DeletionPlan::default();
    for m in matches {
        let id = m.message.id;
        match (m.deletability, revoke) {
            // Unknown: let TDLib decide and report errors if any.
            (None, true) => plan.for_everyone.push(id),
            (None, false) => plan.for_self.push(id),
            (Some(d), true) if d.for_everyone => plan.for_everyone.push(id),
            (Some(d), _) if d.for_self => plan.for_self.push(id),
            (Some(d), false) if d.for_everyone => plan.skipped.push((id, "can only be deleted for everyone")),
            (Some(_), _) => plan.skipped.push((id, "no permission to delete it")),
        }
    }
    plan
}

#[derive(Debug, Default)]
pub struct DeleteReport {
    pub deleted_for_everyone: usize,
    pub deleted_for_self: usize,
    pub failed: Vec<(i64, String)>,
    pub interrupted: bool,
}

impl DeleteReport {
    pub fn deleted(&self) -> usize {
        self.deleted_for_everyone + self.deleted_for_self
    }
}

pub fn execute_plan(tg: &Telegram, chat_id: i64, plan: &DeletionPlan, progress: &dyn Progress) -> Result<DeleteReport> {
    let mut report = DeleteReport::default();
    let total = plan.len();
    let mut done = 0;
    for (ids, revoke) in [(&plan.for_everyone, true), (&plan.for_self, false)] {
        for chunk in ids.chunks(PAGE_SIZE) {
            if signal::cancelled() {
                report.interrupted = true;
                return Ok(report);
            }
            let deleted = match with_flood_wait(progress, || tg.delete_messages(chat_id, chunk, revoke)) {
                Ok(()) => chunk.len(),
                Err(_) if signal::cancelled() => {
                    report.interrupted = true;
                    return Ok(report);
                }
                // Local failures (timeouts) would repeat for every single message.
                Err(err) if err.code == 0 => return Err(err.into()),
                // A single undeletable message fails the whole batch; retry one by one.
                Err(_) => chunk
                    .iter()
                    .filter(
                        |&&id| match with_flood_wait(progress, || tg.delete_messages(chat_id, &[id], revoke)) {
                            Ok(()) => true,
                            Err(err) => {
                                report.failed.push((id, err.message));
                                false
                            }
                        },
                    )
                    .count(),
            };
            if revoke {
                report.deleted_for_everyone += deleted;
            } else {
                report.deleted_for_self += deleted;
            }
            done += chunk.len();
            progress.deleted(done, total);
        }
    }
    Ok(report)
}

/// Runs `op`, sleeping and retrying when Telegram answers with a flood wait.
fn with_flood_wait<T>(progress: &dyn Progress, mut op: impl FnMut() -> Result<T, TdError>) -> Result<T, TdError> {
    let mut waits = 0;
    loop {
        match op() {
            Err(err) if waits < MAX_FLOOD_WAITS && err.retry_after().is_some() => {
                let seconds = err.retry_after().unwrap_or(1);
                progress.waiting(seconds);
                if !signal::sleep(Duration::from_secs(seconds)) {
                    return Err(err);
                }
                waits += 1;
            }
            result => return result,
        }
    }
}
