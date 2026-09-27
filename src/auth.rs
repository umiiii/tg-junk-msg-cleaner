//! Drives TDLib's authorization state machine until the account is logged in.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::client::{Client, TdError};

const STATE_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_ATTEMPTS: usize = 5;

pub struct TdlibParams {
    pub api_id: i32,
    pub api_hash: String,
    pub database_directory: PathBuf,
    pub use_test_dc: bool,
}

impl TdlibParams {
    fn to_request(&self) -> Value {
        json!({
            "@type": "setTdlibParameters",
            "use_test_dc": self.use_test_dc,
            "database_directory": self.database_directory.to_string_lossy(),
            "files_directory": "",
            "database_encryption_key": "",
            "use_file_database": false,
            "use_chat_info_database": true,
            // Needed by searchChatMessages; also caches scanned history.
            "use_message_database": true,
            "use_secret_chats": false,
            "api_id": self.api_id,
            "api_hash": self.api_hash,
            "system_language_code": "en",
            "device_model": "tg-junk-msg-cleaner",
            "system_version": std::env::consts::OS,
            "application_version": env!("CARGO_PKG_VERSION"),
        })
    }
}

/// Supplies the answers needed during login.
pub trait AuthPrompter {
    fn phone_number(&mut self) -> Result<String>;
    /// `info` is an `authenticationCodeInfo` object.
    fn code(&mut self, info: &Value) -> Result<String>;
    fn email_address(&mut self) -> Result<String>;
    /// `info` is an `emailAddressAuthenticationCodeInfo` object.
    fn email_code(&mut self, info: &Value) -> Result<String>;
    fn password(&mut self, hint: &str) -> Result<String>;
    /// Reports a rejected answer before asking again.
    fn retry(&mut self, message: &str);
    fn notice(&mut self, message: &str);
}

/// Logs in, prompting for whatever TDLib asks for. Returns once the client is ready.
pub fn authorize(client: &Client, params: &TdlibParams, prompter: &mut dyn AuthPrompter) -> Result<()> {
    loop {
        let state = client.next_auth_state(STATE_TIMEOUT)?;
        match state["@type"].as_str().unwrap_or_default() {
            "authorizationStateWaitTdlibParameters" => {
                client
                    .request(params.to_request())
                    .context("TDLib rejected the parameters (check --api-id/--api-hash and --data-dir)")?;
            }
            "authorizationStateWaitPhoneNumber" => submit_with_retries(
                prompter,
                |p| p.phone_number(),
                |phone| {
                    client.request(json!({
                        "@type": "setAuthenticationPhoneNumber",
                        "phone_number": phone,
                        "settings": null,
                    }))
                },
            )?,
            "authorizationStateWaitEmailAddress" => submit_with_retries(
                prompter,
                |p| p.email_address(),
                |email| client.request(json!({ "@type": "setAuthenticationEmailAddress", "email_address": email })),
            )?,
            "authorizationStateWaitEmailCode" => {
                let info = &state["code_info"];
                submit_with_retries(
                    prompter,
                    |p| p.email_code(info),
                    |code| {
                        client.request(json!({
                            "@type": "checkAuthenticationEmailCode",
                            "code": { "@type": "emailAddressAuthenticationCode", "code": code },
                        }))
                    },
                )?
            }
            "authorizationStateWaitCode" => {
                let info = &state["code_info"];
                submit_with_retries(
                    prompter,
                    |p| p.code(info),
                    |code| client.request(json!({ "@type": "checkAuthenticationCode", "code": code })),
                )?
            }
            "authorizationStateWaitPassword" => {
                let hint = state["password_hint"].as_str().unwrap_or_default();
                submit_with_retries(
                    prompter,
                    |p| p.password(hint),
                    |password| client.request(json!({ "@type": "checkAuthenticationPassword", "password": password })),
                )?
            }
            "authorizationStateWaitOtherDeviceConfirmation" => {
                let link = state["link"].as_str().unwrap_or_default();
                prompter.notice(&format!("Confirm this login from another device: {link}"));
            }
            "authorizationStateWaitRegistration" => {
                bail!("this phone number has no Telegram account; sign up with an official app first")
            }
            "authorizationStateWaitPremiumPurchase" => {
                bail!("Telegram requires a Premium purchase before this account can log in; use an official app")
            }
            "authorizationStateReady" => return Ok(()),
            "authorizationStateLoggingOut" | "authorizationStateClosing" | "authorizationStateClosed" => {
                bail!("the TDLib session is closing; run the command again")
            }
            other => bail!("unsupported TDLib authorization state {other:?}"),
        }
    }
}

fn submit_with_retries(
    prompter: &mut dyn AuthPrompter,
    mut ask: impl FnMut(&mut dyn AuthPrompter) -> Result<String>,
    submit: impl Fn(&str) -> Result<Value, TdError>,
) -> Result<()> {
    for attempt in 1..=MAX_ATTEMPTS {
        let answer = ask(prompter)?;
        prompter.notice("Contacting Telegram...");
        match submit(answer.trim()) {
            Ok(_) => return Ok(()),
            // 400 means the answer was rejected (wrong code, invalid number, ...).
            Err(err) if err.code == 400 && attempt < MAX_ATTEMPTS => prompter.retry(&describe_auth_error(&err)),
            Err(err) => bail!("login failed: {}", describe_auth_error(&err)),
        }
    }
    unreachable!("the last attempt always returns")
}

fn describe_auth_error(err: &TdError) -> String {
    let hint = match err.message.as_str() {
        "PHONE_NUMBER_INVALID" => "invalid phone number; use the international format, e.g. +8613800138000",
        "PHONE_NUMBER_BANNED" => "this phone number is banned by Telegram",
        "PHONE_CODE_INVALID" => "wrong code",
        "PHONE_CODE_EXPIRED" => "the code has expired; run the command again to get a new one",
        "PASSWORD_HASH_INVALID" => "wrong password",
        "EMAIL_CODE_INVALID" => "wrong email code",
        _ if err.retry_after().is_some() => {
            return format!(
                "too many attempts, Telegram asks to wait {} seconds",
                err.retry_after().unwrap_or(0)
            )
        }
        _ => return err.to_string(),
    };
    format!("{hint} ({})", err.message)
}

/// Human readable description of where the login code was sent.
pub fn describe_code_delivery(info: &Value) -> String {
    let kind = &info["type"];
    let length = kind["length"].as_i64().filter(|&len| len > 0);
    let digits = length.map(|len| format!("{len}-digit ")).unwrap_or_default();
    match kind["@type"].as_str().unwrap_or_default() {
        "authenticationCodeTypeTelegramMessage" => {
            format!("A {digits}code was sent to your Telegram app on another device")
        }
        "authenticationCodeTypeSms" | "authenticationCodeTypeFirebaseAndroid" | "authenticationCodeTypeFirebaseIos" => {
            format!("A {digits}code was sent by SMS")
        }
        "authenticationCodeTypeSmsWord" => "A word was sent by SMS; enter it as the code".to_owned(),
        "authenticationCodeTypeSmsPhrase" => "A phrase was sent by SMS; enter it as the code".to_owned(),
        "authenticationCodeTypeCall" => format!("You will receive a phone call dictating a {digits}code"),
        "authenticationCodeTypeFlashCall" | "authenticationCodeTypeMissedCall" => {
            "You will receive a call; enter the last digits of the calling number".to_owned()
        }
        "authenticationCodeTypeFragment" => {
            format!(
                "A code was sent to your anonymous number on Fragment: {}",
                kind["url"].as_str().unwrap_or_default()
            )
        }
        _ => "A login code was sent".to_owned(),
    }
}
