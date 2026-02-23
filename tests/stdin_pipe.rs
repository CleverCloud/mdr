use std::process::{Command, Stdio};
use std::io::Write;
use std::time::Duration;

/// Helper to get the path to the mdr binary built by cargo test.
fn mdr_bin() -> std::path::PathBuf {
    // cargo test builds the binary in the same target directory
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove "deps"
    path.push("mdr");
    path
}

#[test]
fn stdin_pipe_with_list_backends_exits_successfully() {
    // --list-backends exits before backend runs, proving CLI accepts piped stdin
    let mut child = Command::new(mdr_bin())
        .arg("--list-backends")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mdr");

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"# Hello\n").unwrap();
    }

    let output = child.wait_with_output().expect("failed to wait");
    assert!(output.status.success(), "mdr --list-backends should exit successfully");
}

#[test]
fn stdin_dash_argument_does_not_error_file_not_found() {
    let mut child = Command::new(mdr_bin())
        .arg("-")
        .arg("-b")
        .arg("tui")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mdr");

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"# Test from stdin dash\n").unwrap();
    }

    // The TUI backend may fail without a real terminal, so give it a moment
    // then kill it. The key assertion is that it does NOT fail with "file '-' not found".
    std::thread::sleep(Duration::from_secs(2));
    let _ = child.kill();
    let output = child.wait_with_output().expect("failed to wait");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("file '-' not found"),
        "mdr should read from stdin when '-' is passed, got stderr: {}",
        stderr
    );
}

#[test]
fn stdin_pipe_creates_temp_file() {
    let mut child = Command::new(mdr_bin())
        .arg("-")
        .arg("-b")
        .arg("tui")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mdr");

    let child_pid = child.id();

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"# Temp file test\n").unwrap();
    }

    // Give the process time to write the temp file, then kill it
    std::thread::sleep(Duration::from_secs(2));
    let _ = child.kill();
    let _ = child.wait_with_output();

    // Verify the temp file was created with PID-scoped name
    let tmp_file = std::env::temp_dir().join("mdr").join(format!("stdin-{}.md", child_pid));
    assert!(
        tmp_file.exists(),
        "temp file should be created at {:?}",
        tmp_file
    );

    let content = std::fs::read_to_string(&tmp_file).unwrap();
    assert!(
        content.contains("Temp file test"),
        "temp file should contain piped content, got: {}",
        content
    );

    // Cleanup
    let _ = std::fs::remove_file(&tmp_file);
}

#[test]
fn nonexistent_file_shows_error() {
    let output = Command::new(mdr_bin())
        .arg("this_file_does_not_exist.md")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run mdr");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "should show file not found error, got stderr: {}",
        stderr
    );
}

