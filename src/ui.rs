//! Terminal output: progress indicators, match listings and login prompts.

use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{Local, TimeZone};
use console::style;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;

use crate::auth::{describe_code_delivery, AuthPrompter};
use crate::cleaner::{DeleteReport, DeletionPlan, Match, Progress, ScanOutcome};
use crate::telegram::{ChatInfo, Telegram};

pub fn is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn ensure_terminal(what: &str) -> Result<()> {
    if !is_terminal() {
        bail!("{what} needs an interactive terminal");
    }
    Ok(())
}

pub fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

pub fn confirm(prompt: &str, default: bool) -> Result<bool> {
    Ok(Confirm::with_theme(&theme())
        .with_prompt(prompt)
        .default(default)
        .interact()?)
}

/// A spinner (for scanning) or a bar (for deleting) on stderr; hidden when stderr
/// is not a terminal.
pub struct TerminalProgress {
    bar: ProgressBar,
    label: &'static str,
}

impl TerminalProgress {
    pub fn spinner(label: &'static str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg} {elapsed:.dim}").unwrap());
        bar.set_message(format!("{label}..."));
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar, label }
    }

    pub fn bar(label: &'static str, total: usize) -> Self {
        let bar = ProgressBar::new(total as u64);
        bar.set_style(
            ProgressStyle::with_template("{msg} [{bar:30.cyan/dim}] {pos}/{len} {elapsed:.dim}")
                .unwrap()
                .progress_chars("=> "),
        );
        bar.set_message(label);
        Self { bar, label }
    }

    pub fn finish(self) {
        self.bar.finish_and_clear();
    }
}

impl Progress for TerminalProgress {
    fn scanned(&self, scanned: usize, matched: usize) {
        self.bar.set_message(format!(
            "{}: {scanned} messages examined, {matched} matched",
            self.label
        ));
    }

    fn deleted(&self, done: usize, _total: usize) {
        self.bar.set_message(self.label);
        self.bar.set_position(done as u64);
    }

    fn waiting(&self, seconds: u64) {
        self.bar
            .set_message(format!("{}: Telegram rate limit, waiting {seconds}s", self.label));
    }
}

pub fn format_date(timestamp: i64) -> String {
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|date| date.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "????-??-?? ??:??".to_owned())
}

/// Collapses whitespace and cuts the text to `max_chars` characters.
pub fn preview(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let cut: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

pub fn print_matches(tg: &Telegram, matches: &[Match]) {
    for m in matches {
        let note = match m.deletability {
            Some(d) if !d.for_self && !d.for_everyone => style(" (cannot delete)").red().to_string(),
            Some(d) if !d.for_everyone => style(" (only for you)").yellow().to_string(),
            _ => String::new(),
        };
        println!(
            "{} {} {} {}{}",
            style(format!("#{}", m.message.id)).dim(),
            format_date(m.message.date),
            style(tg.sender_name(m.message.sender)).cyan(),
            style(format!("[{}]", m.keywords.join(", "))).yellow(),
            note,
        );
        println!("    {}", preview(&m.message.text, 160));
    }
}

pub fn print_scan_summary(chat: &ChatInfo, outcome: &ScanOutcome) {
    let partial = if outcome.interrupted {
        " (interrupted, partial results)"
    } else {
        ""
    };
    println!(
        "{} {} messages examined in {}{partial}: {} matched.",
        style("●").cyan(),
        outcome.scanned,
        style(&chat.title).bold(),
        style(outcome.matches.len()).bold(),
    );
}

pub fn print_plan(plan: &DeletionPlan, dry_run: bool) {
    let mut parts = Vec::new();
    if !plan.for_everyone.is_empty() {
        parts.push(format!("{} for everyone", plan.for_everyone.len()));
    }
    if !plan.for_self.is_empty() {
        parts.push(format!("{} only for you", plan.for_self.len()));
    }
    if !parts.is_empty() {
        let verb = if dry_run { "Would delete" } else { "To delete" };
        println!("{} {verb}: {}.", style("●").cyan(), parts.join(", "));
    }
    if !plan.skipped.is_empty() {
        println!(
            "{} {} matched message(s) will be kept:",
            style("●").yellow(),
            plan.skipped.len()
        );
        for (id, reason) in plan.skipped.iter().take(10) {
            println!("    #{id}: {reason}");
        }
        if plan.skipped.len() > 10 {
            println!("    ...and {} more", plan.skipped.len() - 10);
        }
    }
}

pub fn print_report(report: &DeleteReport) {
    let mut line = format!("{} Deleted {} message(s)", style("✔").green(), report.deleted());
    if report.deleted_for_self > 0 {
        line.push_str(&format!(" ({} only for you)", report.deleted_for_self));
    }
    println!("{line}.");
    if report.interrupted {
        println!("{} Stopped early because of Ctrl-C.", style("●").yellow());
    }
    if !report.failed.is_empty() {
        println!(
            "{} {} message(s) could not be deleted:",
            style("✘").red(),
            report.failed.len()
        );
        for (id, error) in report.failed.iter().take(10) {
            println!("    #{id}: {error}");
        }
    }
}

fn ensure_login_terminal() -> Result<()> {
    if !is_terminal() {
        bail!("not logged in yet; run `tg-junk-cleaner login` in an interactive terminal first");
    }
    Ok(())
}

/// Asks for login details on the terminal.
pub struct TerminalPrompter {
    phone: Option<String>,
    theme: ColorfulTheme,
}

impl TerminalPrompter {
    pub fn new(phone: Option<String>) -> Self {
        Self { phone, theme: theme() }
    }

    fn ask(&self, prompt: &str) -> Result<String> {
        ensure_login_terminal()?;
        Ok(Input::<String>::with_theme(&self.theme)
            .with_prompt(prompt)
            .interact()?)
    }
}

impl AuthPrompter for TerminalPrompter {
    fn phone_number(&mut self) -> Result<String> {
        match self.phone.take() {
            Some(phone) => Ok(phone),
            None => self.ask("Phone number (international format, e.g. +8613800138000)"),
        }
    }

    fn code(&mut self, info: &Value) -> Result<String> {
        eprintln!("{}", describe_code_delivery(info));
        self.ask("Login code")
    }

    fn email_address(&mut self) -> Result<String> {
        eprintln!("Telegram asks for a login email address for this account.");
        self.ask("Email address")
    }

    fn email_code(&mut self, info: &Value) -> Result<String> {
        let pattern = info["email_address_pattern"].as_str().unwrap_or("your email address");
        eprintln!("A code was sent to {pattern}.");
        self.ask("Email code")
    }

    fn password(&mut self, hint: &str) -> Result<String> {
        ensure_login_terminal()?;
        let prompt = if hint.is_empty() {
            "Two-step verification password".to_owned()
        } else {
            format!("Two-step verification password (hint: {hint})")
        };
        Ok(Password::with_theme(&self.theme).with_prompt(prompt).interact()?)
    }

    fn retry(&mut self, message: &str) {
        eprintln!("{} {message}", style("✘").red());
    }

    fn notice(&mut self, message: &str) {
        eprintln!("{}", style(message).dim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_are_flattened_and_cut() {
        assert_eq!(preview("a\n\n b  c", 10), "a b c");
        assert_eq!(preview("日赚千元加微信详聊", 5), "日赚千元…");
    }
}
