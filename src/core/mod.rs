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

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

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

/// Which colour scheme the rendering should assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Work it out from the environment, and fall back to dark.
    #[default]
    Auto,
    Dark,
    Light,
}

impl Theme {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Theme::Auto),
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            _ => None,
        }
    }
}

static THEME: AtomicU8 = AtomicU8::new(0);

pub fn set_theme(theme: Theme) {
    THEME.store(
        match theme {
            Theme::Auto => 0,
            Theme::Dark => 1,
            Theme::Light => 2,
        },
        Ordering::Relaxed,
    );
}

/// Read back by the terminal backend, which is the only one that has to pick a
/// palette itself; the two graphical backends follow the system colour scheme.
#[cfg(feature = "tui-backend")]
pub fn theme() -> Theme {
    match THEME.load(Ordering::Relaxed) {
        1 => Theme::Dark,
        2 => Theme::Light,
        _ => Theme::Auto,
    }
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
