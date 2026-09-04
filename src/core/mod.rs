pub mod config;
pub mod icon;
pub mod image_validation;
pub mod markdown;
pub mod mermaid;
pub mod net;
pub mod paths;
pub mod search;
pub mod slug;
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

static OFFLINE: AtomicBool = AtomicBool::new(false);

/// Turn off every network access mdr would otherwise make (remote images).
pub fn set_offline(v: bool) {
    OFFLINE.store(v, Ordering::Relaxed);
}

pub fn offline() -> bool {
    OFFLINE.load(Ordering::Relaxed)
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
