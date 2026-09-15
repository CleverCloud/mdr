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
/// Shared palette and type scale: only the two graphical backends draw a page.
#[cfg(any(feature = "egui-backend", feature = "webview-backend"))]
pub mod style;
#[cfg(any(
    feature = "egui-backend",
    feature = "webview-backend",
    feature = "tui-backend"
))]
pub mod svg;
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

#[cfg(any(
    feature = "egui-backend",
    feature = "webview-backend",
    feature = "tui-backend"
))]
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
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
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

/// Where a relative path in the document resolves from, when the document did
/// not come from a file the reader named.
///
/// `cat README.md | mdr` writes the document to a temp file, so its images
/// would otherwise be looked for next to that temp file — which is why a piped
/// README showed a broken image where its logo should be. The directory mdr was
/// run from is the one the reader meant.
static DOCUMENT_BASE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub fn set_document_base(dir: std::path::PathBuf) {
    let _ = DOCUMENT_BASE.set(dir);
}

/// Where the backends resolve a document's relative paths from.
///
/// The override when there is one, the document's own directory otherwise, and
/// the working directory when even that is unavailable.
#[cfg(any(
    feature = "egui-backend",
    feature = "webview-backend",
    feature = "tui-backend"
))]
pub fn document_base_dir(document: &std::path::Path) -> std::path::PathBuf {
    base_dir_of(
        document,
        DOCUMENT_BASE.get().map(std::path::PathBuf::as_path),
    )
}

/// The decision behind [`document_base_dir`], with the override passed in.
#[cfg(any(
    feature = "egui-backend",
    feature = "webview-backend",
    feature = "tui-backend",
    test
))]
fn base_dir_of(
    document: &std::path::Path,
    override_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    if let Some(dir) = override_dir {
        return dir.to_path_buf();
    }
    document.parent().map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        std::path::Path::to_path_buf,
    )
}

/// The colour scheme every backend renders with.
///
/// `Auto` means "ask the environment": the terminal backend reads `COLORFGBG`,
/// the two graphical ones follow the system setting. `Dark` and `Light` settle
/// it, which is the whole point of `--theme` — it used to reach the terminal
/// backend only, so `--theme light` did nothing at all in `gui` and `web`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_document_resolves_relative_paths_next_to_itself() {
        assert_eq!(
            base_dir_of(Path::new("/home/me/project/README.md"), None),
            PathBuf::from("/home/me/project")
        );
    }

    #[test]
    fn a_piped_document_resolves_them_where_mdr_was_run() {
        // `cat README.md | mdr` writes the document to a temp file. Its images
        // were written against the directory the reader was in, not against
        // that temp file — which is why a piped README showed a broken image
        // where its logo should be.
        assert_eq!(
            base_dir_of(
                Path::new("/var/folders/tmp/mdr/stdin-1234.md"),
                Some(Path::new("/home/me/project")),
            ),
            PathBuf::from("/home/me/project")
        );
    }

    #[test]
    fn a_document_with_no_parent_falls_back_to_the_working_directory() {
        assert_eq!(
            base_dir_of(Path::new("/"), None),
            std::env::current_dir().unwrap_or_default()
        );
    }
}
