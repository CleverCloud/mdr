pub mod markdown;
pub mod mermaid;
pub mod search;
pub mod toc;
pub mod watcher;

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(v: bool) {
    VERBOSE.store(v, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Log a message if verbose mode is enabled.
#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => {
        if $crate::core::verbose() {
            eprintln!("[mdr] {}", format!($($arg)*));
        }
    };
}
