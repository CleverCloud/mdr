//! `--export html`: the page of the web backend, printed instead of shown.
#![cfg(feature = "webview-backend")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Point a child at a config directory of its own, so a test run never
/// creates or rewrites the developer's `~/.config/mdr/config.kdl`.
fn isolate_config<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("APPDATA", home.join("appdata"))
}

fn mdr_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // the test binary
    path.pop(); // "deps"
    path.push("mdr");
    path
}

/// Run mdr with `args` in a sandbox, feeding `stdin` to it.
fn run(args: &[&str], stdin: &str) -> Output {
    let sandbox = tempfile::tempdir().expect("a temp directory");
    let mut cmd = Command::new(mdr_bin());
    let mut child = isolate_config(&mut cmd, sandbox.path())
        .current_dir(sandbox.path())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mdr");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().expect("failed to wait")
}

#[test]
fn export_html_prints_the_page_of_a_piped_document() {
    let output = run(&["--export", "html", "-"], "# Hello\n\nWorld.\n");
    assert!(output.status.success(), "{output:?}");
    let page = String::from_utf8(output.stdout).unwrap();
    assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
    assert!(page.contains(r#"<h1 id="hello">Hello</h1>"#), "{page}");
    // The table of contents comes with it, as in the window.
    assert!(page.contains(r##"<a href="#hello">Hello</a>"##), "{page}");
}

#[test]
fn export_html_keeps_the_page_offline_by_default() {
    let output = run(&["--export", "html", "-"], "# Hello\n");
    let page = String::from_utf8(output.stdout).unwrap();
    assert!(page.contains("connect-src 'none'"), "{page}");
}

#[test]
fn allow_connect_relaxes_connect_src_and_nothing_else() {
    let output = run(&["--export", "html", "--allow-connect", "-"], "# Hello\n");
    assert!(output.status.success(), "{output:?}");
    let page = String::from_utf8(output.stdout).unwrap();
    assert!(page.contains("connect-src 'self'"), "{page}");
    for directive in ["default-src 'none'", "img-src data:", "form-action 'none'"] {
        assert!(page.contains(directive), "CSP misses {directive}: {page}");
    }
}

#[test]
fn allow_connect_is_refused_without_export() {
    // Without `--export` there is no origin for `'self'` to mean, and the
    // flag must not slip through to a backend that opens a window.
    let output = run(&["--allow-connect", "-"], "# Hello\n");
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--export"), "{stderr}");
}

#[test]
fn export_html_inlines_images_relative_to_the_document() {
    let docs = tempfile::tempdir().expect("a temp directory");
    let logo = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/logo-128.png");
    std::fs::copy(logo, docs.path().join("logo.png")).unwrap();
    let doc = docs.path().join("doc.md");
    std::fs::write(&doc, "![logo](logo.png)\n").unwrap();

    // Run from elsewhere: the image resolves against the document, not the cwd.
    let output = run(&["--export", "html", doc.to_str().unwrap()], "");
    assert!(output.status.success(), "{output:?}");
    let page = String::from_utf8(output.stdout).unwrap();
    assert!(page.contains("src=\"data:image/png;base64,"), "{page}");
}
