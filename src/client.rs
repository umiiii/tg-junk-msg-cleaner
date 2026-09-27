//! A small synchronous TDLib client on top of [`TdBackend`].
//!
//! A background thread owns `td_receive` and routes every answer to the request
//! that is waiting for it (matched by `@extra`). Authorization state updates are
//! forwarded on a channel; all other updates are ignored because this tool only
//! needs request/response calls.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::tdjson::TdBackend;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(15);

/// An error returned by TDLib, or a local failure while talking to it (code 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TdError {
    pub code: i32,
    pub message: String,
}

impl TdError {
    fn from_value(value: &Value) -> Self {
        Self {
            code: value["code"].as_i64().unwrap_or(0) as i32,
            message: value["message"].as_str().unwrap_or("unknown error").to_owned(),
        }
    }

    pub fn local(message: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
        }
    }

    /// For flood-wait errors ("Too Many Requests: retry after N"), the number of
    /// seconds to wait before retrying.
    pub fn retry_after(&self) -> Option<u64> {
        if self.code != 429 {
            return None;
        }
        let tail = self.message.rsplit("retry after ").next()?;
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        Some(digits.parse().unwrap_or(5))
    }
}

impl fmt::Display for TdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.code == 0 {
            f.write_str(&self.message)
        } else {
            write!(f, "{} (TDLib error {})", self.message, self.code)
        }
    }
}

impl std::error::Error for TdError {}

type Pending = Arc<Mutex<HashMap<u64, Sender<Value>>>>;

pub struct Client {
    backend: Arc<dyn TdBackend>,
    client_id: i32,
    pending: Pending,
    next_extra: AtomicU64,
    auth_states: Receiver<Value>,
    last_auth_state: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    receiver: Option<JoinHandle<()>>,
    request_timeout: Duration,
}

impl Client {
    pub fn new(backend: Arc<dyn TdBackend>) -> Self {
        let client_id = backend.create_client_id();
        let pending: Pending = Arc::default();
        let last_auth_state = Arc::new(Mutex::new(String::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (auth_tx, auth_rx) = mpsc::channel();

        let receiver = {
            let backend = Arc::clone(&backend);
            let pending = Arc::clone(&pending);
            let last_auth_state = Arc::clone(&last_auth_state);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("tdlib-receiver".into())
                .spawn(move || receive_loop(&*backend, client_id, &pending, &auth_tx, &last_auth_state, &stop))
                .expect("failed to spawn the TDLib receiver thread")
        };

        let client = Self {
            backend,
            client_id,
            pending,
            next_extra: AtomicU64::new(1),
            auth_states: auth_rx,
            last_auth_state,
            stop,
            receiver: Some(receiver),
            request_timeout: REQUEST_TIMEOUT,
        };
        // TDLib instantiates the client lazily on its first request; after that
        // it starts sending authorization state updates.
        client.send_raw(&json!({ "@type": "getOption", "name": "version" }));
        client
    }

    fn send_raw(&self, request: &Value) {
        self.backend.send(self.client_id, &request.to_string());
    }

    /// Sends a request and blocks until TDLib answers it.
    pub fn request(&self, mut request: Value) -> Result<Value, TdError> {
        let extra = self.next_extra.fetch_add(1, Ordering::Relaxed);
        let method = request["@type"].as_str().unwrap_or("request").to_owned();
        request["@extra"] = json!(extra);

        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(extra, tx);
        self.send_raw(&request);

        match rx.recv_timeout(self.request_timeout) {
            Ok(response) if response["@type"] == "error" => Err(TdError::from_value(&response)),
            Ok(response) => Ok(response),
            Err(_) => {
                self.pending.lock().unwrap().remove(&extra);
                Err(TdError::local(format!(
                    "timed out waiting for TDLib to answer {method} (is the network reachable?)"
                )))
            }
        }
    }

    /// Waits for the next `authorizationState*` object.
    pub fn next_auth_state(&self, timeout: Duration) -> Result<Value, TdError> {
        self.auth_states
            .recv_timeout(timeout)
            .map_err(|_| TdError::local("timed out waiting for TDLib (is the network reachable?)"))
    }

    /// Closes the TDLib instance so that its databases are flushed, and stops the
    /// receiver thread. Called automatically on drop.
    pub fn close(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };
        if *self.last_auth_state.lock().unwrap() != "authorizationStateClosed" {
            self.send_raw(&json!({ "@type": "close" }));
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                match self.auth_states.recv_timeout(remaining) {
                    Ok(state) if state["@type"] == "authorizationStateClosed" => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
        self.stop.store(true, Ordering::Relaxed);
        let _ = receiver.join();
    }

    #[cfg(test)]
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

fn receive_loop(
    backend: &dyn TdBackend,
    client_id: i32,
    pending: &Pending,
    auth_states: &Sender<Value>,
    last_auth_state: &Mutex<String>,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Relaxed) {
        let Some(raw) = backend.receive(0.5) else {
            continue;
        };
        let Ok(mut value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if value["@client_id"]
            .as_i64()
            .is_some_and(|id| id != i64::from(client_id))
        {
            continue;
        }
        if let Some(extra) = value["@extra"].as_u64() {
            if let Some(waiter) = pending.lock().unwrap().remove(&extra) {
                let _ = waiter.send(value);
            }
            continue;
        }
        if value["@type"] == "updateAuthorizationState" {
            let state = value["authorization_state"].take();
            *last_auth_state.lock().unwrap() = state["@type"].as_str().unwrap_or_default().to_owned();
            let _ = auth_states.send(state);
        }
    }
}

/// Configures TDLib's internal logging. Must be called before creating a client.
pub fn configure_logging(backend: &dyn TdBackend, verbosity: i32, log_file: Option<&std::path::Path>) {
    let _ = backend.execute(&json!({ "@type": "setLogVerbosityLevel", "new_verbosity_level": verbosity }).to_string());
    if let Some(path) = log_file {
        let _ = backend.execute(
            &json!({
                "@type": "setLogStream",
                "log_stream": {
                    "@type": "logStreamFile",
                    "path": path.to_string_lossy(),
                    "max_file_size": 10 * 1024 * 1024,
                    "redirect_stderr": false,
                },
            })
            .to_string(),
        );
    }
}

/// Returns the version of the loaded TDLib, e.g. "1.8.67".
pub fn tdlib_version(backend: &dyn TdBackend) -> Option<String> {
    let raw = backend.execute(&json!({ "@type": "getOption", "name": "version" }).to_string())?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value["value"].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flood_wait() {
        let err = TdError {
            code: 429,
            message: "Too Many Requests: retry after 17".into(),
        };
        assert_eq!(err.retry_after(), Some(17));
        let err = TdError {
            code: 400,
            message: "MESSAGE_ID_INVALID".into(),
        };
        assert_eq!(err.retry_after(), None);
        let err = TdError {
            code: 429,
            message: "Too Many Requests".into(),
        };
        assert_eq!(err.retry_after(), Some(5));
    }
}
