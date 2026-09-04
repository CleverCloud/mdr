pub mod config;
// Only the graphical backends set a window icon.
#[cfg(any(feature = "egui-backend", feature = "webview-backend"))]
pub mod icon;
pub mod image_validation;
// The HTML pipeline exists for the webview backend; egui and tui render from
// the Markdown source directly.
#[cfg(feature = "webview-backend")]
pub mod markdown;
pub mod mermaid;
// Remote images are only inlined by the two graphical backends; tui fetches
// them itself, through the image crate.
#[cfg(any(feature = "egui-backend", feature = "webview-backend"))]
pub mod net;
pub mod paths;
// Untrusted HTML only reaches a real HTML engine in the webview backend.
#[cfg(feature = "webview-backend")]
pub mod sanitize;
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

#[cfg(any(feature = "egui-backend", feature = "webview-backend"))]
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
