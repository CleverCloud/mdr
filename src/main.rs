mod backend;
mod core;

use clap::Parser;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime};

#[derive(Parser)]
#[command(
    name = "mdr",
    version,
    about = "Lightweight Markdown viewer with live reload"
)]
struct Cli {
    /// Markdown file to render (use '-' or pipe via stdin)
    file: Option<PathBuf>,

    /// Rendering backend to use: egui (native GUI), webview (HTML), tui (terminal)
    #[arg(short, long, value_parser = parse_backend)]
    backend: Option<String>,

    /// Enable verbose logging (image resolution, mermaid rendering, etc.)
    #[arg(short, long)]
    verbose: bool,

    /// Never access the network: remote images are not downloaded
    #[arg(long)]
    offline: bool,

    /// Path to config file [default: ~/.config/mdr/config.kdl]
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// List available backends and exit
    #[arg(long)]
    list_backends: bool,

    /// Create a default config file and exit
    #[arg(long)]
    init: bool,
}

fn print_backends() {
    fn status(compiled: bool) -> &'static str {
        if compiled {
            "✓ compiled"
        } else {
            "✗ not compiled"
        }
    }
    eprintln!("Available backends:");
    eprintln!(
        "  egui      Native GUI window (OpenGL)            [{}]",
        status(cfg!(feature = "egui-backend"))
    );
    eprintln!(
        "  webview   System webview (WebKit/WebView2)      [{}]",
        status(cfg!(feature = "webview-backend"))
    );
    eprintln!(
        "  tui       Terminal UI with image support         [{}]",
        status(cfg!(feature = "tui-backend"))
    );
    eprintln!("  auto      Auto-detect best available (default)");
}

fn parse_backend(s: &str) -> Result<String, String> {
    match s {
        "auto" | "egui" | "webview" | "tui" => Ok(s.to_string()),
        _ => Err(format!(
            "unknown backend '{}', expected 'auto', 'egui', 'webview', or 'tui'",
            s
        )),
    }
}

/// Auto-detect the best backend for the current environment.
fn detect_backend() -> &'static str {
    // If no DISPLAY/WAYLAND and we have a TTY → TUI
    // If SSH session → TUI
    // Otherwise → egui (or first available GUI backend)
    let is_ssh = std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok();
    let has_display = std::env::var("DISPLAY").is_ok()
        || std::env::var("WAYLAND_DISPLAY").is_ok()
        || cfg!(target_os = "macos")
        || cfg!(target_os = "windows");

    if is_ssh {
        #[cfg(feature = "tui-backend")]
        return "tui";
    }

    if has_display {
        #[cfg(feature = "egui-backend")]
        return "egui";
        #[cfg(all(not(feature = "egui-backend"), feature = "webview-backend"))]
        return "webview";
    }

    #[cfg(feature = "tui-backend")]
    return "tui";

    #[cfg(not(feature = "tui-backend"))]
    {
        #[cfg(feature = "egui-backend")]
        return "egui";
        #[cfg(all(not(feature = "egui-backend"), feature = "webview-backend"))]
        return "webview";
        #[cfg(not(any(feature = "egui-backend", feature = "webview-backend")))]
        {
            eprintln!("Error: no backend compiled");
            process::exit(1);
        }
    }
}

/// Temp files left by runs that could not clean up after themselves (SIGKILL)
/// are removed once they are older than this.
const STALE_TMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Directory holding the temp files created for piped input.
fn stdin_tmp_dir() -> PathBuf {
    std::env::temp_dir().join("mdr")
}

/// Create `dir` with owner-only permissions. If it already exists, refuse
/// anything that is not a plain directory we own — in a shared /tmp it could be
/// a symlink planted by another user.
#[cfg(unix)]
fn ensure_tmp_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // symlink_metadata: a symlink here must be rejected, not followed.
            let meta = std::fs::symlink_metadata(dir)?;
            if !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "exists and is not a directory",
                ));
            }
            if meta.uid() != unsafe { libc::getuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "is owned by another user",
                ));
            }
            if meta.permissions().mode() & 0o077 != 0 {
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(not(unix))]
fn ensure_tmp_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// A per-run, hard-to-guess name so two instances never collide, even if the
/// system recycles a pid.
fn stdin_tmp_name() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let rand = RandomState::new().build_hasher().finish();
    format!("stdin-{}-{:016x}.md", process::id(), rand)
}

/// Write `content` to a fresh file in `dir`, created readable by its owner only
/// (the mode is set at creation: the content is never world-readable).
fn write_stdin_tmp_file(dir: &Path, content: &str) -> io::Result<PathBuf> {
    let path = dir.join(stdin_tmp_name());
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&path)?;
    file.write_all(content.as_bytes())?;
    Ok(path)
}

/// Best-effort housekeeping: drop stdin temp files older than [`STALE_TMP_AGE`].
/// Never reports an error — it must not keep mdr from starting.
fn cleanup_stale_tmp_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("stdin-") || !name.ends_with(".md") {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if now
            .duration_since(modified)
            .is_ok_and(|age| age >= STALE_TMP_AGE)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Read stdin and write it to a temp file, returning its path. The file is
/// deleted when mdr exits (see [`main`]); backends keep watching it until then.
fn read_stdin_to_tmpfile() -> Result<PathBuf, String> {
    let mut content = String::new();
    io::stdin()
        .lock()
        .read_to_string(&mut content)
        .map_err(|e| format!("failed to read from stdin: {}", e))?;

    let dir = stdin_tmp_dir();
    ensure_tmp_dir(&dir)
        .map_err(|e| format!("failed to create temp directory '{}': {}", dir.display(), e))?;
    cleanup_stale_tmp_files(&dir);

    let path = write_stdin_tmp_file(&dir, &content)
        .map_err(|e| format!("failed to write temp file in '{}': {}", dir.display(), e))?;
    crate::vlog!("piped input stored in {}", path.display());
    Ok(path)
}

fn main() {
    let mut tmp_file: Option<PathBuf> = None;
    let code = run(&mut tmp_file);
    // process::exit() below runs no destructor, so the temp file holding piped
    // input is removed here, explicitly, whatever the exit code.
    if let Some(path) = &tmp_file {
        let _ = std::fs::remove_file(path);
    }
    process::exit(code);
}

/// Body of `main`, returning the process exit code so that the caller can clean
/// up before exiting.
fn run(tmp_file: &mut Option<PathBuf>) -> i32 {
    let cli = Cli::parse();

    if cli.list_backends {
        print_backends();
        return 0;
    }

    if cli.init {
        let path = cli
            .config
            .clone()
            .unwrap_or_else(core::config::default_path);
        return match core::config::write_default(&path) {
            Ok(()) => {
                eprintln!("Created config file: {}", path.display());
                0
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                1
            }
        };
    }

    // Load config (explicit path errors if missing; default path is optional)
    let cfg_path = cli
        .config
        .clone()
        .unwrap_or_else(core::config::default_path);
    let cfg = if cli.config.is_some() && !cfg_path.exists() {
        eprintln!("Error: config file '{}' not found", cfg_path.display());
        return 1;
    } else {
        core::config::load(&cfg_path).unwrap_or_else(|e| {
            eprintln!("mdr: config error ({}): {}", cfg_path.display(), e);
            core::config::Config::default()
        })
    };

    core::set_verbose(cli.verbose || cfg.verbose.unwrap_or(false));
    core::set_offline(cli.offline || cfg.offline.unwrap_or(false));

    let from_stdin = |tmp_file: &mut Option<PathBuf>| match read_stdin_to_tmpfile() {
        Ok(path) => {
            *tmp_file = Some(path.clone());
            Ok(path)
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            Err(1)
        }
    };

    let file = match cli.file {
        Some(f) if f.as_os_str() == "-" => match from_stdin(tmp_file) {
            Ok(path) => path,
            Err(code) => return code,
        },
        Some(f) => {
            if !f.exists() {
                eprintln!("Error: file '{}' not found", f.display());
                return 1;
            }
            f
        }
        None => {
            if io::stdin().is_terminal() {
                eprintln!("Error: missing required argument <FILE>");
                eprintln!("Usage: mdr <FILE> [OPTIONS]");
                eprintln!("       cat file.md | mdr [OPTIONS]");
                eprintln!("Try 'mdr --help' for more information.");
                return 1;
            }
            match from_stdin(tmp_file) {
                Ok(path) => path,
                Err(code) => return code,
            }
        }
    };

    let backend_str = cli
        .backend
        .or(cfg.backend)
        .unwrap_or_else(|| "auto".to_string());
    let backend = if backend_str == "auto" {
        detect_backend()
    } else {
        backend_str.as_str()
    };

    let result = match backend {
        #[cfg(feature = "egui-backend")]
        "egui" => backend::egui::run(file),

        #[cfg(not(feature = "egui-backend"))]
        "egui" => {
            eprintln!("Error: egui backend not compiled. Rebuild with --features egui-backend");
            return 1;
        }

        #[cfg(feature = "webview-backend")]
        "webview" => backend::webview::run(file),

        #[cfg(not(feature = "webview-backend"))]
        "webview" => {
            eprintln!(
                "Error: webview backend not compiled. Rebuild with --features webview-backend"
            );
            return 1;
        }

        #[cfg(feature = "tui-backend")]
        "tui" => backend::tui::run(file),

        #[cfg(not(feature = "tui-backend"))]
        "tui" => {
            eprintln!("Error: tui backend not compiled. Rebuild with --features tui-backend");
            return 1;
        }

        _ => unreachable!(),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_offline_flag() {
        let cli = Cli::try_parse_from(["mdr", "--offline", "file.md"]).unwrap();
        assert!(cli.offline);

        let cli = Cli::try_parse_from(["mdr", "file.md"]).unwrap();
        assert!(!cli.offline);
    }

    #[test]
    fn write_stdin_tmp_file_writes_content_under_a_unique_name() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_stdin_tmp_file(dir.path(), "# piped\n").unwrap();
        let b = write_stdin_tmp_file(dir.path(), "# piped\n").unwrap();

        assert_ne!(a, b, "two runs must not collide on the same file name");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "# piped\n");

        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&format!("stdin-{}-", process::id())));
        assert!(name.ends_with(".md"));
    }

    #[cfg(unix)]
    #[test]
    fn write_stdin_tmp_file_creates_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = write_stdin_tmp_file(dir.path(), "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_tmp_dir_creates_an_owner_only_directory() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("mdr");
        ensure_tmp_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {:o}", mode);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_tmp_dir_tightens_a_loose_existing_directory() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("mdr");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        ensure_tmp_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {:o}", mode);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_tmp_dir_refuses_a_symlink() {
        let base = tempfile::tempdir().unwrap();
        let target = base.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let dir = base.path().join("mdr");
        std::os::unix::fs::symlink(&target, &dir).unwrap();

        assert!(
            ensure_tmp_dir(&dir).is_err(),
            "a symlinked temp directory must be refused, not followed"
        );
    }

    #[test]
    fn ensure_tmp_dir_refuses_a_regular_file() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("mdr");
        std::fs::write(&dir, "not a directory").unwrap();

        assert!(ensure_tmp_dir(&dir).is_err());
    }

    #[test]
    fn cleanup_stale_tmp_files_only_removes_old_stdin_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = SystemTime::now() - Duration::from_secs(48 * 3600);

        let write_aged = |name: &str, aged: bool| {
            let path = dir.path().join(name);
            std::fs::write(&path, "x").unwrap();
            if aged {
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
            path
        };

        let stale = write_aged("stdin-1-deadbeef.md", true);
        let fresh = write_aged("stdin-2-cafebabe.md", false);
        let unrelated = write_aged("notes.md", true);
        let other_ext = write_aged("stdin-3.txt", true);

        cleanup_stale_tmp_files(dir.path());

        assert!(!stale.exists(), "old stdin temp files must be removed");
        assert!(fresh.exists(), "recent stdin temp files must be kept");
        assert!(unrelated.exists(), "other files must never be touched");
        assert!(other_ext.exists(), "other files must never be touched");
    }

    #[test]
    fn cleanup_stale_tmp_files_ignores_a_missing_directory() {
        cleanup_stale_tmp_files(Path::new("/nonexistent/mdr-no-such-dir"));
    }
}
