use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::cleaner::ScanMode;

/// Delete Telegram messages that match your keywords in a single chat, using your
/// own account through TDLib.
///
/// Run without a command for the guided interactive mode.
#[derive(Debug, Parser)]
#[command(name = "tg-junk-cleaner", version, after_help = EXAMPLES)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

const EXAMPLES: &str = "\
Examples:
  tg-junk-cleaner                                   guided interactive mode
  tg-junk-cleaner login                             log in once and keep the session
  tg-junk-cleaner chats --search spam               find the ID of a chat
  tg-junk-cleaner clean -c -1001234567890 -k airdrop -k 加微信 --dry-run
  tg-junk-cleaner clean -c @my_group -f keywords.txt --yes";

#[derive(Debug, Args)]
#[command(next_help_heading = "Account and TDLib options")]
pub struct GlobalArgs {
    /// Telegram API ID, from https://my.telegram.org/apps
    #[arg(long, env = "TG_API_ID", global = true, value_name = "ID")]
    pub api_id: Option<i32>,

    /// Telegram API hash, from https://my.telegram.org/apps
    #[arg(
        long,
        env = "TG_API_HASH",
        global = true,
        value_name = "HASH",
        hide_env_values = true
    )]
    pub api_hash: Option<String>,

    /// Phone number of the account, in international format (asked when needed)
    #[arg(long, env = "TG_PHONE", global = true, value_name = "PHONE", hide_env_values = true)]
    pub phone: Option<String>,

    /// Directory for the TDLib session and saved settings
    /// [default: <user data dir>/tg-junk-msg-cleaner]
    #[arg(long, env = "TG_CLEANER_DATA_DIR", global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Path to TDLib's JSON library (libtdjson.so / libtdjson.dylib / tdjson.dll)
    #[arg(long, env = "TDJSON_PATH", global = true, value_name = "PATH")]
    pub tdjson: Option<PathBuf>,

    /// TDLib log verbosity (0-5); the log is written to tdlib.log in the data directory
    #[arg(long, global = true, default_value_t = 1, value_name = "LEVEL",
          value_parser = clap::value_parser!(i32).range(0..=5))]
    pub tdlib_log_level: i32,

    /// Use Telegram's test servers (for development)
    #[arg(long, global = true, hide = true)]
    pub test_dc: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Guided mode: pick a chat, enter keywords, preview and confirm (the default)
    Interactive(InteractiveArgs),
    /// Delete messages matching keywords in ONE chat, configured with arguments
    Clean(CleanArgs),
    /// List your chats and their IDs
    Chats(ChatsArgs),
    /// Log in and keep the session for later runs
    Login,
    /// Log out, deleting the local session and saved API credentials
    Logout,
}

#[derive(Debug, Default, Args)]
pub struct InteractiveArgs {
    /// Only show what would be deleted; never delete anything
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// The one chat to clean: numeric ID, @username, t.me link or exact title
    #[arg(short, long, value_name = "CHAT")]
    pub chat: String,

    /// A keyword to look for; repeat for several keywords
    #[arg(short = 'k', long = "keyword", value_name = "KEYWORD")]
    pub keywords: Vec<String>,

    /// Read keywords from a file, one per line (lines starting with # are ignored)
    #[arg(short = 'f', long, value_name = "FILE")]
    pub keywords_file: Option<PathBuf>,

    /// Treat keywords as regular expressions
    #[arg(long)]
    pub regex: bool,

    /// Match case-sensitively (default: ignore case)
    #[arg(long)]
    pub case_sensitive: bool,

    /// Only match messages that contain all keywords (default: any keyword)
    #[arg(long)]
    pub match_all: bool,

    /// How to find candidate messages
    #[arg(long, value_enum, default_value_t = ScanMode::Full)]
    pub mode: ScanMode,

    /// Examine at most this many messages, newest first
    #[arg(long, value_name = "N")]
    pub limit: Option<NonZeroUsize>,

    /// Only list the matching messages; do not delete anything
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Delete without asking for confirmation
    #[arg(short = 'y', long, conflicts_with = "dry_run")]
    pub yes: bool,

    /// Delete messages only for yourself instead of for everyone
    /// (private chats and basic groups only)
    #[arg(long)]
    pub for_me_only: bool,

    /// Print only the summary instead of every matching message
    #[arg(short, long)]
    pub quiet: bool,
}

#[derive(Debug, Args)]
pub struct ChatsArgs {
    /// Maximum number of chats to show
    #[arg(long, default_value_t = 100, value_name = "N")]
    pub limit: usize,

    /// Only show chats whose title contains this text
    #[arg(short, long, value_name = "TEXT")]
    pub search: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn clean_accepts_only_one_chat() {
        let err = Cli::try_parse_from(["tg-junk-cleaner", "clean", "-c", "1", "-c", "2", "-k", "x"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
        let cli = Cli::try_parse_from(["tg-junk-cleaner", "clean", "-c", "1", "-k", "a", "-k", "b", "-n"]).unwrap();
        let Some(Command::Clean(args)) = cli.command else {
            panic!("expected clean")
        };
        assert_eq!(args.keywords, ["a", "b"]);
        assert!(args.dry_run);
    }
}
