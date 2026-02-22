mod backend;
mod core;

use clap::Parser;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "mdr", version, about = "Lightweight Markdown viewer with live reload")]
struct Cli {
    /// Markdown file to render
    file: PathBuf,

    /// Rendering backend to use [default: auto-detect]
    #[arg(short, long, default_value = "auto", value_parser = parse_backend)]
    backend: String,

    /// Enable verbose logging (image resolution, mermaid rendering, etc.)
    #[arg(short, long)]
    verbose: bool,
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

fn main() {
    let cli = Cli::parse();
    core::set_verbose(cli.verbose);

    if !cli.file.exists() {
        eprintln!("Error: file '{}' not found", cli.file.display());
        process::exit(1);
    }

    let backend = if cli.backend == "auto" {
        detect_backend()
    } else {
        cli.backend.as_str()
    };

    let result = match backend {
        #[cfg(feature = "egui-backend")]
        "egui" => backend::egui::run(cli.file),

        #[cfg(not(feature = "egui-backend"))]
        "egui" => {
            eprintln!("Error: egui backend not compiled. Rebuild with --features egui-backend");
            process::exit(1);
        }

        #[cfg(feature = "webview-backend")]
        "webview" => backend::webview::run(cli.file),

        #[cfg(not(feature = "webview-backend"))]
        "webview" => {
            eprintln!("Error: webview backend not compiled. Rebuild with --features webview-backend");
            process::exit(1);
        }

        #[cfg(feature = "tui-backend")]
        "tui" => backend::tui::run(cli.file),

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
