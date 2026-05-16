mod backend;
mod core;

use clap::Parser;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "mdr", version, about = "Lightweight Markdown viewer with live reload")]
struct Cli {
    /// Markdown file to render (use '-' or pipe via stdin)
    file: Option<PathBuf>,

    /// Rendering backend to use: egui (native GUI), webview (HTML), tui (terminal)
    #[arg(short, long, value_parser = parse_backend)]
    backend: Option<String>,

    /// Enable verbose logging (image resolution, mermaid rendering, etc.)
    #[arg(short, long)]
    verbose: bool,

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
        if compiled { "✓ compiled" } else { "✗ not compiled" }
    }
    eprintln!("Available backends:");
    eprintln!("  egui      Native GUI window (OpenGL)            [{}]", status(cfg!(feature = "egui-backend")));
    eprintln!("  webview   System webview (WebKit/WebView2)      [{}]", status(cfg!(feature = "webview-backend")));
    eprintln!("  tui       Terminal UI with image support         [{}]", status(cfg!(feature = "tui-backend")));
    eprintln!("  auto      Auto-detect best available (default)");
}

fn parse_backend(s: &str) -> Result<String, String> {
    match s {
        "auto" | "egui" | "webview" | "tui" => Ok(s.to_string()),
        _ => Err(format!("unknown backend '{}', expected 'auto', 'egui', 'webview', or 'tui'", s)),
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

/// Read stdin and write to a temp file, returning its path.
fn read_stdin_to_tmpfile() -> PathBuf {
    let mut content = String::new();
    io::stdin().lock().read_to_string(&mut content).unwrap_or_else(|e| {
        eprintln!("Error: failed to read from stdin: {}", e);
        process::exit(1);
    });
    let tmp_dir = std::env::temp_dir().join("mdr");
    std::fs::create_dir_all(&tmp_dir).unwrap_or_else(|e| {
        eprintln!("Error: failed to create temp directory: {}", e);
        process::exit(1);
    });
    let tmp_file = tmp_dir.join(format!("stdin-{}.md", process::id()));
    std::fs::write(&tmp_file, &content).unwrap_or_else(|e| {
        eprintln!("Error: failed to write temp file: {}", e);
        process::exit(1);
    });
    tmp_file
}

fn main() {
    let cli = Cli::parse();

    if cli.list_backends {
        print_backends();
        process::exit(0);
    }

    if cli.init {
        let path = cli.config.clone().unwrap_or_else(core::config::default_path);
        match core::config::write_default(&path) {
            Ok(()) => {
                eprintln!("Created config file: {}", path.display());
                process::exit(0);
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                process::exit(1);
            }
        }
    }

    // Load config (explicit path errors if missing; default path is optional)
    let cfg_path = cli.config.clone().unwrap_or_else(core::config::default_path);
    let cfg = if cli.config.is_some() && !cfg_path.exists() {
        eprintln!("Error: config file '{}' not found", cfg_path.display());
        process::exit(1);
    } else {
        core::config::load(&cfg_path).unwrap_or_else(|e| {
            eprintln!("mdr: config error ({}): {}", cfg_path.display(), e);
            core::config::Config::default()
        })
    };

    core::set_verbose(cli.verbose || cfg.verbose.unwrap_or(false));

    let file = match cli.file {
        Some(f) if f.as_os_str() == "-" => read_stdin_to_tmpfile(),
        Some(f) => {
            if !f.exists() {
                eprintln!("Error: file '{}' not found", f.display());
                process::exit(1);
            }
            f
        }
        None => {
            if io::stdin().is_terminal() {
                eprintln!("Error: missing required argument <FILE>");
                eprintln!("Usage: mdr <FILE> [OPTIONS]");
                eprintln!("       cat file.md | mdr [OPTIONS]");
                eprintln!("Try 'mdr --help' for more information.");
                process::exit(1);
            }
            read_stdin_to_tmpfile()
        }
    };

    let backend_str = cli.backend
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
            process::exit(1);
        }

        #[cfg(feature = "webview-backend")]
        "webview" => backend::webview::run(file),

        #[cfg(not(feature = "webview-backend"))]
        "webview" => {
            eprintln!("Error: webview backend not compiled. Rebuild with --features webview-backend");
            process::exit(1);
        }

        #[cfg(feature = "tui-backend")]
        "tui" => backend::tui::run(file),

        #[cfg(not(feature = "tui-backend"))]
        "tui" => {
            eprintln!("Error: tui backend not compiled. Rebuild with --features tui-backend");
            process::exit(1);
        }

        _ => unreachable!(),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
