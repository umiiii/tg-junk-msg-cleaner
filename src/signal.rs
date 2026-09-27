//! Ctrl-C handling: during a scan or deletion the first Ctrl-C stops after the
//! current step (so TDLib is closed cleanly); anywhere else, or on a second
//! Ctrl-C, the program exits immediately.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static CANCELLABLE: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn install_handler() {
    let result = ctrlc::set_handler(|| {
        if CANCELLABLE.load(Ordering::SeqCst) && !CANCELLED.swap(true, Ordering::SeqCst) {
            eprintln!("\nStopping after the current step... (press Ctrl-C again to quit immediately)");
        } else {
            let _ = console::Term::stderr().show_cursor();
            std::process::exit(130);
        }
    });
    if let Err(err) = result {
        eprintln!("warning: could not install the Ctrl-C handler: {err}");
    }
}

pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

/// Makes Ctrl-C request a graceful stop while the guard is alive.
pub struct CancellableSection(());

impl CancellableSection {
    pub fn enter() -> Self {
        CANCELLED.store(false, Ordering::SeqCst);
        CANCELLABLE.store(true, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for CancellableSection {
    fn drop(&mut self) {
        CANCELLABLE.store(false, Ordering::SeqCst);
    }
}

/// Sleeps for `duration` unless cancelled first. Returns false if cancelled.
pub fn sleep(duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if cancelled() {
            return false;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(200)));
    }
    !cancelled()
}
