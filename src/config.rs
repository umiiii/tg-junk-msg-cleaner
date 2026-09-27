//! Data directory and saved API credentials.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const CREDENTIALS_FILE: &str = "api_credentials.json";

/// The `api_id` / `api_hash` pair from https://my.telegram.org/apps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCredentials {
    pub api_id: i32,
    pub api_hash: String,
}

pub fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .map(|dir| dir.join("tg-junk-msg-cleaner"))
        .unwrap_or_else(|| PathBuf::from(".tg-junk-msg-cleaner"))
}

/// Creates the data directory, readable only by the current user.
pub fn prepare_data_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create data directory {}", dir.display()))?;
    restrict_permissions(dir, 0o700)
}

pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CREDENTIALS_FILE)
}

pub fn load_credentials(data_dir: &Path) -> Result<Option<ApiCredentials>> {
    let path = credentials_path(data_dir);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).map(Some).with_context(|| {
            format!(
                "{} is not valid; delete it and enter the credentials again",
                path.display()
            )
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub fn save_credentials(data_dir: &Path, credentials: &ApiCredentials) -> Result<PathBuf> {
    let path = credentials_path(data_dir);
    fs::write(&path, serde_json::to_string_pretty(credentials)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    restrict_permissions(&path, 0o600)?;
    Ok(path)
}

/// Removes saved credentials. Returns whether a file was removed.
pub fn remove_credentials(data_dir: &Path) -> Result<bool> {
    let path = credentials_path(data_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_credentials() {
        let dir = std::env::temp_dir().join(format!("tg-cleaner-config-test-{}", std::process::id()));
        prepare_data_dir(&dir).unwrap();
        assert!(load_credentials(&dir).unwrap().is_none());
        save_credentials(
            &dir,
            &ApiCredentials {
                api_id: 12345,
                api_hash: "abc".into(),
            },
        )
        .unwrap();
        let loaded = load_credentials(&dir).unwrap().unwrap();
        assert_eq!((loaded.api_id, loaded.api_hash.as_str()), (12345, "abc"));
        assert!(remove_credentials(&dir).unwrap());
        assert!(!remove_credentials(&dir).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }
}
