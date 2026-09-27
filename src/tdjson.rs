//! Runtime binding to TDLib's JSON interface (`libtdjson`).
//!
//! The library is loaded with `dlopen` at startup instead of being linked at build
//! time, so the tool builds without TDLib installed and users can point it at any
//! TDLib build they have.

use std::ffi::{c_char, c_double, c_int, CStr, CString};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use libloading::Library;

/// The four entry points of TDLib's JSON client API.
///
/// Implemented by the real shared library, and by an in-memory fake in tests.
pub trait TdBackend: Send + Sync {
    fn create_client_id(&self) -> i32;
    fn send(&self, client_id: i32, request: &str);
    /// Waits up to `timeout` seconds for the next response or update of any client.
    /// Must not be called from more than one thread at a time.
    fn receive(&self, timeout: f64) -> Option<String>;
    /// Runs a synchronous TDLib request (only a few methods support this).
    fn execute(&self, request: &str) -> Option<String>;
}

type CreateClientIdFn = unsafe extern "C" fn() -> c_int;
type SendFn = unsafe extern "C" fn(c_int, *const c_char);
type ReceiveFn = unsafe extern "C" fn(c_double) -> *const c_char;
type ExecuteFn = unsafe extern "C" fn(*const c_char) -> *const c_char;

pub struct TdJson {
    create_client_id: CreateClientIdFn,
    send: SendFn,
    receive: ReceiveFn,
    execute: ExecuteFn,
    location: PathBuf,
    // Keeps the function pointers above valid. Never unloaded: TDLib's worker
    // threads may outlive the last client, and unloading their code would crash.
    _library: ManuallyDrop<Library>,
}

impl TdJson {
    /// Loads `libtdjson` from `explicit`, or from the default locations when `None`.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let candidates = match explicit {
            Some(path) => vec![path.to_path_buf()],
            None => default_candidates(),
        };
        let mut failures = Vec::new();
        for candidate in &candidates {
            // SAFETY: loading TDLib runs no initialisation code with preconditions.
            match unsafe { Library::new(candidate) } {
                Ok(library) => return Self::from_library(library, candidate.clone()),
                Err(err) => failures.push(format!("  {}: {}", candidate.display(), error_chain(&err))),
            }
        }
        Err(anyhow!(
            "could not load the TDLib JSON library (libtdjson).\n\
             Tried:\n{}\n\n\
             Build or install TDLib (https://github.com/tdlib/td, see README) and pass \
             --tdjson <path to libtdjson> or set TDJSON_PATH.",
            failures.join("\n")
        ))
    }

    fn from_library(library: Library, location: PathBuf) -> Result<Self> {
        // SAFETY: the signatures match TDLib's td_json_client.h.
        unsafe {
            let missing = |name: &str, err: libloading::Error| {
                anyhow!(
                    "{} does not export {name} ({err}); TDLib 1.8.6 or newer is required",
                    location.display()
                )
            };
            let create_client_id = *library
                .get::<CreateClientIdFn>(b"td_create_client_id\0")
                .map_err(|e| missing("td_create_client_id", e))?;
            let send = *library.get::<SendFn>(b"td_send\0").map_err(|e| missing("td_send", e))?;
            let receive = *library
                .get::<ReceiveFn>(b"td_receive\0")
                .map_err(|e| missing("td_receive", e))?;
            let execute = *library
                .get::<ExecuteFn>(b"td_execute\0")
                .map_err(|e| missing("td_execute", e))?;
            Ok(Self {
                create_client_id,
                send,
                receive,
                execute,
                location,
                _library: ManuallyDrop::new(library),
            })
        }
    }

    pub fn location(&self) -> &Path {
        &self.location
    }
}

impl TdBackend for TdJson {
    fn create_client_id(&self) -> i32 {
        // SAFETY: no preconditions.
        unsafe { (self.create_client_id)() }
    }

    fn send(&self, client_id: i32, request: &str) {
        let request = CString::new(request).expect("TDLib requests never contain NUL bytes");
        // SAFETY: TDLib copies the request before returning.
        unsafe { (self.send)(client_id, request.as_ptr()) }
    }

    fn receive(&self, timeout: f64) -> Option<String> {
        // SAFETY: the returned string stays valid until the next td_receive call,
        // and it is copied before returning.
        unsafe { owned_string((self.receive)(timeout)) }
    }

    fn execute(&self, request: &str) -> Option<String> {
        let request = CString::new(request).expect("TDLib requests never contain NUL bytes");
        // SAFETY: as for `receive`, the result is copied immediately.
        unsafe { owned_string((self.execute)(request.as_ptr())) }
    }
}

/// libloading puts the useful part (the loader's message) in the error source.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    text
}

unsafe fn owned_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

fn default_candidates() -> Vec<PathBuf> {
    let file_name = PathBuf::from(libloading::library_filename("tdjson"));
    let mut candidates = Vec::new();
    // Next to the executable, which is convenient for self-contained installs.
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join(&file_name));
    }
    // A bare file name makes the OS loader search its default paths (LD_LIBRARY_PATH etc).
    candidates.push(file_name);
    if cfg!(target_os = "macos") {
        candidates.push("/opt/homebrew/lib/libtdjson.dylib".into());
        candidates.push("/usr/local/lib/libtdjson.dylib".into());
    } else if cfg!(all(unix, not(target_os = "macos"))) {
        candidates.push("/usr/local/lib/libtdjson.so".into());
    }
    candidates
}
