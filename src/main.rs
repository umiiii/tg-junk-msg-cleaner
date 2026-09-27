mod auth;
mod cleaner;
mod cli;
mod client;
mod commands;
mod config;
mod interactive;
mod matcher;
mod message;
mod session;
mod signal;
mod tdjson;
mod telegram;
mod ui;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod real_tdlib_tests;

use std::process::ExitCode;

use clap::Parser;
use console::style;

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    signal::install_handler();

    let result = match cli.command {
        None => interactive::run(&cli.global, Default::default()),
        Some(Command::Interactive(args)) => interactive::run(&cli.global, args),
        Some(Command::Clean(args)) => commands::clean(&cli.global, args),
        Some(Command::Chats(args)) => commands::chats(&cli.global, args),
        Some(Command::Login) => commands::login(&cli.global),
        Some(Command::Logout) => commands::logout(&cli.global),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if is_interrupted(&err) => {
            let _ = console::Term::stderr().show_cursor();
            ExitCode::from(130)
        }
        Err(err) => {
            eprintln!("{} {err:#}", style("error:").red().bold());
            ExitCode::FAILURE
        }
    }
}

/// Ctrl-C inside a prompt surfaces as an `Interrupted` I/O error.
fn is_interrupted(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let io = cause
            .downcast_ref::<std::io::Error>()
            .or_else(|| match cause.downcast_ref::<dialoguer::Error>() {
                Some(dialoguer::Error::IO(io)) => Some(io),
                _ => None,
            });
        io.is_some_and(|io| io.kind() == std::io::ErrorKind::Interrupted)
    })
}
