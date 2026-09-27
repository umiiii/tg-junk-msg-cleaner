//! The non-interactive commands, and the scan/delete steps shared with the
//! interactive mode.

use std::num::NonZeroUsize;

use anyhow::{anyhow, bail, Result};
use console::style;
use serde_json::Value;

use crate::auth::AuthPrompter;
use crate::cleaner::{self, DeleteReport, DeletionPlan, ScanOptions, ScanOutcome};
use crate::cli::{ChatsArgs, CleanArgs, GlobalArgs};
use crate::config;
use crate::matcher::{read_keywords_file, MatchOptions, Matcher};
use crate::session;
use crate::signal::CancellableSection;
use crate::telegram::{ChatInfo, ChatRef, Telegram};
use crate::ui::{self, TerminalProgress};

pub fn clean(global: &GlobalArgs, args: CleanArgs) -> Result<()> {
    // Validate everything that does not need Telegram before logging in.
    let chat_ref = ChatRef::parse(&args.chat)?;
    let mut keywords = args.keywords.clone();
    if let Some(path) = &args.keywords_file {
        keywords.extend(read_keywords_file(path)?);
    }
    let matcher = Matcher::new(
        keywords,
        MatchOptions {
            regex: args.regex,
            case_sensitive: args.case_sensitive,
            require_all: args.match_all,
        },
    )?;
    if args.mode == cleaner::ScanMode::Search && args.regex {
        bail!("--mode search only works with plain keywords; use --mode full with --regex");
    }
    if !args.dry_run && !args.yes && !ui::is_terminal() {
        bail!("not running in a terminal, so deletion cannot be confirmed; pass --yes or --dry-run");
    }

    let tg = session::open(global)?;
    let chat = tg.resolve_chat(&chat_ref)?;
    if args.for_me_only && !chat.kind.supports_delete_for_self() {
        bail!(
            "messages in a {} can only be deleted for everyone; drop --for-me-only",
            chat.kind
        );
    }
    print_target(&chat, &matcher);

    let outcome = scan(
        &tg,
        &chat,
        &matcher,
        ScanOptions {
            mode: args.mode,
            limit: args.limit.map(NonZeroUsize::get),
        },
    )?;
    if !args.quiet {
        ui::print_matches(&tg, &outcome.matches);
    }
    ui::print_scan_summary(&chat, &outcome);
    if outcome.matches.is_empty() {
        return Ok(());
    }
    let plan = cleaner::plan_deletion(&outcome.matches, !args.for_me_only);
    ui::print_plan(&plan, args.dry_run);

    if args.dry_run {
        println!("{} Dry run: nothing was deleted.", style("●").cyan());
        return Ok(());
    }
    if outcome.interrupted {
        println!(
            "{} The scan was interrupted, so nothing was deleted.",
            style("●").yellow()
        );
        return Ok(());
    }
    if plan.is_empty() {
        return Ok(());
    }
    if !args.yes && !confirm_deletion(&chat, &plan)? {
        println!("Cancelled; nothing was deleted.");
        return Ok(());
    }
    let report = delete(&tg, &chat, &plan)?;
    ui::print_report(&report);
    if !report.failed.is_empty() {
        bail!("{} message(s) could not be deleted", report.failed.len());
    }
    Ok(())
}

pub fn print_target(chat: &ChatInfo, matcher: &Matcher) {
    let options = matcher.options();
    let mut flags = vec![if options.regex { "regex" } else { "plain text" }];
    flags.push(if options.case_sensitive {
        "case-sensitive"
    } else {
        "ignore case"
    });
    flags.push(if options.require_all {
        "all keywords"
    } else {
        "any keyword"
    });
    eprintln!("{} {chat}", style("Chat:").bold());
    eprintln!(
        "{} {} ({})",
        style("Keywords:").bold(),
        matcher
            .keywords()
            .iter()
            .map(|k| format!("{k:?}"))
            .collect::<Vec<_>>()
            .join(", "),
        flags.join(", ")
    );
}

pub fn scan(tg: &Telegram, chat: &ChatInfo, matcher: &Matcher, options: ScanOptions) -> Result<ScanOutcome> {
    let _cancellable = CancellableSection::enter();
    let progress = TerminalProgress::spinner("Scanning");
    let outcome = cleaner::scan(tg, chat.id, matcher, &options, &progress);
    progress.finish();
    outcome
}

pub fn confirm_deletion(chat: &ChatInfo, plan: &DeletionPlan) -> Result<bool> {
    ui::confirm(
        &format!(
            "Permanently delete {} message(s) from \"{}\"? This cannot be undone",
            plan.len(),
            chat.title
        ),
        false,
    )
}

pub fn delete(tg: &Telegram, chat: &ChatInfo, plan: &DeletionPlan) -> Result<DeleteReport> {
    let _cancellable = CancellableSection::enter();
    let progress = TerminalProgress::bar("Deleting", plan.len());
    let report = cleaner::execute_plan(tg, chat.id, plan, &progress);
    progress.finish();
    report
}

pub fn chats(global: &GlobalArgs, args: ChatsArgs) -> Result<()> {
    let tg = session::open(global)?;
    let search = args.search.as_deref().map(str::to_lowercase);
    // Search through all chats, not only the first `limit`.
    let load = if search.is_some() { 10_000 } else { args.limit };
    let chats: Vec<ChatInfo> = tg
        .chats(load)?
        .into_iter()
        .filter(|chat| search.as_deref().is_none_or(|s| chat.title.to_lowercase().contains(s)))
        .take(args.limit)
        .collect();
    if chats.is_empty() {
        eprintln!("No chats found.");
        return Ok(());
    }
    println!("{:>16}  {:<10}  TITLE", "ID", "TYPE");
    for chat in chats {
        println!("{:>16}  {:<10}  {}", chat.id, chat.kind.to_string(), chat.title);
    }
    Ok(())
}

pub fn login(global: &GlobalArgs) -> Result<()> {
    let tg = session::open(global)?;
    let me = tg.me()?;
    println!("{} Logged in as {} (user id {}).", style("✔").green(), me.name, me.id);
    Ok(())
}

pub fn logout(global: &GlobalArgs) -> Result<()> {
    let data_dir = session::data_dir(global);
    let mut prompter = NoLogin;
    match session::open_with(global, &mut prompter) {
        Ok(tg) => {
            tg.log_out()?;
            drop(tg); // waits until TDLib has removed the session
            println!("{} Logged out; the local session was removed.", style("✔").green());
        }
        Err(err) if err.is::<NotLoggedIn>() => println!("Not logged in."),
        Err(err) => return Err(err),
    }
    if config::remove_credentials(&data_dir)? {
        println!("Removed the saved API credentials.");
    }
    Ok(())
}

#[derive(Debug)]
struct NotLoggedIn;

impl std::fmt::Display for NotLoggedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not logged in")
    }
}

impl std::error::Error for NotLoggedIn {}

/// Used by `logout`: gives up as soon as TDLib asks for login details.
struct NoLogin;

impl AuthPrompter for NoLogin {
    fn phone_number(&mut self) -> Result<String> {
        Err(anyhow!(NotLoggedIn))
    }
    fn code(&mut self, _: &Value) -> Result<String> {
        Err(anyhow!(NotLoggedIn))
    }
    fn email_address(&mut self) -> Result<String> {
        Err(anyhow!(NotLoggedIn))
    }
    fn email_code(&mut self, _: &Value) -> Result<String> {
        Err(anyhow!(NotLoggedIn))
    }
    fn password(&mut self, _: &str) -> Result<String> {
        Err(anyhow!(NotLoggedIn))
    }
    fn retry(&mut self, _: &str) {}
    fn notice(&mut self, _: &str) {}
}
