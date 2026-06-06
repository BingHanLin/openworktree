//! Process-wide shutdown flag, set on Ctrl+C / termination.
//!
//! A single handler is shared by every running child (one-shot and fan-out), so
//! the handler is installed at most once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static INIT: Once = Once::new();

/// Install the signal handler (idempotent).
pub fn install() {
    INIT.call_once(|| {
        let _ = ctrlc::set_handler(|| SHUTDOWN.store(true, Ordering::SeqCst));
    });
}

/// Whether a shutdown has been requested.
pub fn requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}
