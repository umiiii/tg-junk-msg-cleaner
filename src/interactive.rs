//! The guided mode: every setting is asked for with prompts.

use anyhow::Result;
use console::style;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{FuzzySelect, Input, Select};

use crate::cleaner::{self, ScanMode, ScanOptions};
use crate::cli::{GlobalArgs, InteractiveArgs};
use crate::commands;
use crate::matcher::{MatchOptions, Matcher};
use crate::session;
use crate::telegram::{ChatInfo, ChatRef, Telegram};
use crate::ui::{self, TerminalProgress};

const CHATS_TO_OFFER: usize = 500;
const MATCHES_SHOWN_FIRST: usize = 20;

pub fn run(global: &GlobalArgs, args: InteractiveArgs) -> Result<()> {
    ui::ensure_terminal("Interactive mode (use the `clean` command for scripts)")?;
    let theme = ui::theme();
    eprintln!("{}", style("Telegram junk message cleaner").bold());
    if args.dry_run {
        eprintln!("{}", style("Dry run: nothing will be deleted.").yellow());
    }

    let tg = session::open(global)?;
    let me = tg.me()?;
    eprintln!("{} Logged in as {}", style("✔").green(), me.name);

    let chat = pick_chat(&tg, &theme)?;
    let matcher = ask_matcher(&theme)?;
    let mode = if matcher.options().regex {
        ScanMode::Full
    } else {
        let choice = Select::with_theme(&theme)
            .with_prompt("How should messages be searched?")
            .items([
                "Scan the whole chat history (exact; slower for very large chats)",
                "Use Telegram's server search (faster; may miss matches inside words)",
            ])
            .default(0)
            .interact()?;
        if choice == 0 {
            ScanMode::Full
        } else {
            ScanMode::Search
        }
    };

    eprintln!();
    commands::print_target(&chat, &matcher);
    let outcome = commands::scan(&tg, &chat, &matcher, ScanOptions { mode, limit: None })?;
    show_matches(&tg, &outcome.matches)?;
    ui::print_scan_summary(&chat, &outcome);
    if outcome.matches.is_empty() {
        return Ok(());
    }
    if outcome.interrupted {
        println!(
            "{} The scan was interrupted, so nothing will be deleted.",
            style("●").yellow()
        );
        return Ok(());
    }

    let revoke = if args.dry_run || !chat.kind.supports_delete_for_self() {
        true
    } else {
        let choice = Select::with_theme(&theme)
            .with_prompt("Delete the messages for whom?")
            .items(["For everyone (where allowed, otherwise only for me)", "Only for me"])
            .default(0)
            .interact()?;
        choice == 0
    };
    let plan = cleaner::plan_deletion(&outcome.matches, revoke);
    ui::print_plan(&plan, args.dry_run);
    if args.dry_run {
        println!("{} Dry run: nothing was deleted.", style("●").cyan());
        return Ok(());
    }
    if plan.is_empty() {
        return Ok(());
    }
    if !commands::confirm_deletion(&chat, &plan)? {
        println!("Cancelled; nothing was deleted.");
        return Ok(());
    }
    let report = commands::delete(&tg, &chat, &plan)?;
    ui::print_report(&report);
    Ok(())
}

fn pick_chat(tg: &Telegram, theme: &ColorfulTheme) -> Result<ChatInfo> {
    let progress = TerminalProgress::spinner("Loading chats");
    let chats = tg.chats(CHATS_TO_OFFER);
    progress.finish();
    let chats = chats?;

    let mut items = vec!["› Enter a chat ID, @username or t.me link".to_owned()];
    items.extend(chats.iter().map(|c| format!("{}  ·  {}  ·  {}", c.title, c.kind, c.id)));
    let choice = FuzzySelect::with_theme(theme)
        .with_prompt("Which chat do you want to clean? (type to filter)")
        .items(&items)
        .default(if chats.is_empty() { 0 } else { 1 })
        .max_length(15)
        .interact()?;
    if choice > 0 {
        return Ok(chats[choice - 1].clone());
    }
    loop {
        let input: String = Input::with_theme(theme).with_prompt("Chat").interact()?;
        match ChatRef::parse(&input).and_then(|reference| tg.resolve_chat(&reference)) {
            Ok(chat) => return Ok(chat),
            Err(err) => eprintln!("{} {err:#}", style("✘").red()),
        }
    }
}

fn ask_matcher(theme: &ColorfulTheme) -> Result<Matcher> {
    let kind = Select::with_theme(theme)
        .with_prompt("How should keywords be matched?")
        .items([
            "Plain text, ignoring case (recommended)",
            "Plain text, case-sensitive",
            "Regular expressions, ignoring case",
            "Regular expressions, case-sensitive",
        ])
        .default(0)
        .interact()?;
    let regex = kind >= 2;
    let case_sensitive = kind % 2 == 1;

    eprintln!("Enter one keyword per line; press Enter on an empty line when done.");
    let mut keywords: Vec<String> = Vec::new();
    loop {
        let input: String = Input::with_theme(theme)
            .with_prompt(format!("Keyword #{}", keywords.len() + 1))
            .allow_empty(true)
            .validate_with(move |input: &String| -> Result<(), String> {
                match input.trim() {
                    pattern if regex && !pattern.is_empty() => {
                        regex::Regex::new(pattern).map(drop).map_err(|err| err.to_string())
                    }
                    _ => Ok(()),
                }
            })
            .interact()?;
        let input = input.trim();
        if !input.is_empty() {
            keywords.push(input.to_owned());
        } else if keywords.is_empty() {
            eprintln!("Enter at least one keyword.");
        } else {
            break;
        }
    }

    let require_all = keywords.len() > 1
        && Select::with_theme(theme)
            .with_prompt("Which messages should match?")
            .items([
                "Messages containing ANY of the keywords",
                "Messages containing ALL of the keywords",
            ])
            .default(0)
            .interact()?
            == 1;
    Matcher::new(
        keywords,
        MatchOptions {
            regex,
            case_sensitive,
            require_all,
        },
    )
}

fn show_matches(tg: &Telegram, matches: &[cleaner::Match]) -> Result<()> {
    if matches.len() <= MATCHES_SHOWN_FIRST {
        ui::print_matches(tg, matches);
        return Ok(());
    }
    ui::print_matches(tg, &matches[..MATCHES_SHOWN_FIRST]);
    let rest = matches.len() - MATCHES_SHOWN_FIRST;
    if ui::confirm(&format!("Show the other {rest} matching messages?"), false)? {
        ui::print_matches(tg, &matches[MATCHES_SHOWN_FIRST..]);
    }
    Ok(())
}
