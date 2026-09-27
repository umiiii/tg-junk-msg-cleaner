//! Loading TDLib and logging in, shared by every command.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Result};
use console::style;
use dialoguer::Input;

use crate::auth::{authorize, AuthPrompter, TdlibParams};
use crate::cli::GlobalArgs;
use crate::client::{configure_logging, tdlib_version, Client};
use crate::config::{self, ApiCredentials};
use crate::tdjson::TdJson;
use crate::telegram::Telegram;
use crate::ui::{self, TerminalPrompter};

const MIN_TDLIB_VERSION: (u32, u32, u32) = (1, 8, 6);

pub fn data_dir(global: &GlobalArgs) -> PathBuf {
    global.data_dir.clone().unwrap_or_else(config::default_data_dir)
}

/// Loads TDLib and logs in, asking for the phone number, code and password if needed.
pub fn open(global: &GlobalArgs) -> Result<Telegram> {
    let mut prompter = TerminalPrompter::new(global.phone.clone());
    open_with(global, &mut prompter)
}

pub fn open_with(global: &GlobalArgs, prompter: &mut dyn AuthPrompter) -> Result<Telegram> {
    let data_dir = data_dir(global);
    config::prepare_data_dir(&data_dir)?;
    let credentials = resolve_credentials(global, &data_dir)?;

    let library = TdJson::load(global.tdjson.as_deref())?;
    let version = tdlib_version(&library).unwrap_or_default();
    check_version(&version, &library)?;
    configure_logging(&library, global.tdlib_log_level, Some(&data_dir.join("tdlib.log")));
    eprintln!(
        "{}",
        style(format!(
            "TDLib {version} from {}, data in {}",
            library.location().display(),
            data_dir.display()
        ))
        .dim()
    );

    let client = Client::new(Arc::new(library));
    let params = TdlibParams {
        api_id: credentials.api_id,
        api_hash: credentials.api_hash,
        database_directory: data_dir.join("tdlib"),
        use_test_dc: global.test_dc,
    };
    authorize(&client, &params, prompter)?;
    Ok(Telegram::new(client))
}

fn check_version(version: &str, library: &TdJson) -> Result<()> {
    let parsed: Vec<u32> = version.split('.').map_while(|part| part.parse().ok()).collect();
    if let [major, minor, patch, ..] = parsed[..] {
        if (major, minor, patch) < MIN_TDLIB_VERSION {
            let (a, b, c) = MIN_TDLIB_VERSION;
            bail!(
                "{} is TDLib {version}, but {a}.{b}.{c} or newer is required",
                library.location().display()
            );
        }
    }
    Ok(())
}

/// Takes the API credentials from the arguments/environment or the saved file, and
/// otherwise asks for them (offering to save them).
fn resolve_credentials(global: &GlobalArgs, data_dir: &std::path::Path) -> Result<ApiCredentials> {
    let saved = config::load_credentials(data_dir)?;
    let api_id = global.api_id.or(saved.as_ref().map(|c| c.api_id));
    let api_hash = global.api_hash.clone().or(saved.map(|c| c.api_hash));
    if let (Some(api_id), Some(api_hash)) = (api_id, api_hash.clone()) {
        return Ok(ApiCredentials { api_id, api_hash });
    }
    if !ui::is_terminal() {
        bail!(
            "Telegram API credentials are missing. Create them at https://my.telegram.org/apps, \
             then pass --api-id and --api-hash or set TG_API_ID and TG_API_HASH"
        );
    }

    eprintln!(
        "This tool logs in with your own Telegram API credentials.\n\
         Create them at {} (\"API development tools\").",
        style("https://my.telegram.org/apps").underlined()
    );
    let theme = ui::theme();
    let api_id = match api_id {
        Some(id) => id,
        None => Input::<i32>::with_theme(&theme)
            .with_prompt("api_id")
            .validate_with(|id: &i32| {
                if *id > 0 {
                    Ok(())
                } else {
                    Err("api_id is a positive number")
                }
            })
            .interact()?,
    };
    let api_hash = match api_hash {
        Some(hash) => hash,
        None => Input::<String>::with_theme(&theme)
            .with_prompt("api_hash")
            .validate_with(|hash: &String| {
                let hash = hash.trim();
                if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    Ok(())
                } else {
                    Err("api_hash is 32 hexadecimal characters")
                }
            })
            .interact()?
            .trim()
            .to_owned(),
    };
    let credentials = ApiCredentials { api_id, api_hash };
    let path = config::credentials_path(data_dir);
    if ui::confirm(&format!("Save them to {} for next time?", path.display()), true)? {
        config::save_credentials(data_dir, &credentials)?;
    }
    Ok(credentials)
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_ordering() {
        assert!((1, 8, 5) < super::MIN_TDLIB_VERSION);
        assert!((1, 8, 67) >= super::MIN_TDLIB_VERSION);
        assert!((1, 9, 0) >= super::MIN_TDLIB_VERSION);
    }
}
