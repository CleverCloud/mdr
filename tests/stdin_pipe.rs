use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

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
    assert!(
        output.status.success(),
        "mdr --list-backends should exit successfully"
    );
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

/// List the `stdin-*.md` files left in `<tmpdir>/mdr`.
fn stdin_temp_files(mdr_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(mdr_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("stdin-") && n.ends_with(".md"))
        })
        .collect()
}

/// Run `mdr` with `content` piped on stdin and an isolated TMPDIR, and return
/// (exit success, stderr).
fn run_piped(tmpdir: &Path, args: &[&str], content: &[u8]) -> (bool, String) {
    let mut child = Command::new(mdr_bin())
        .args(args)
        // `std::env::temp_dir()` reads TMPDIR on Unix but TMP then TEMP on
        // Windows, so all three have to be set or the child writes to the real
        // temp directory and the test loses its isolation.
        .env("TMPDIR", tmpdir)
        .env("TMP", tmpdir)
        .env("TEMP", tmpdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mdr");

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(content);
    }

    let output = child.wait_with_output().expect("failed to wait");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn stdin_pipe_temp_file_is_created_then_removed_on_exit() {
    let tmp = tempfile::tempdir().unwrap();
    // Verbose mode reports the temp file it just created, which lets us assert
    // both that it was created and that it is gone once mdr exits.
    let (_, stderr) = run_piped(tmp.path(), &["-", "-b", "tui", "-v"], b"# Temp file test\n");

    let path: PathBuf = stderr
        .lines()
        .find_map(|l| l.split_once("piped input stored in "))
        .map(|(_, p)| PathBuf::from(p.trim()))
        .unwrap_or_else(|| panic!("mdr -v should report the stdin temp file, got: {}", stderr));

    let mdr_dir = tmp.path().join("mdr");
    assert_eq!(
        path.parent(),
        Some(mdr_dir.as_path()),
        "temp file should live in <tmpdir>/mdr, got {:?}",
        path
    );
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("stdin-") && name.ends_with(".md"),
        "unexpected temp file name: {}",
        name
    );
    assert!(
        !path.exists(),
        "temp file {:?} should be removed when mdr exits",
        path
    );
    assert!(
        stdin_temp_files(&mdr_dir).is_empty(),
        "no stdin temp file should be left behind: {:?}",
        stdin_temp_files(&mdr_dir)
    );
}

#[cfg(unix)]
#[test]
fn temp_dir_is_created_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let mdr_dir = tmp.path().join("mdr");

    let (_, _stderr) = run_piped(tmp.path(), &["-", "-b", "tui"], b"# perms\n");

    let mode = std::fs::metadata(&mdr_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "<tmpdir>/mdr should be created with mode 0700, got {:o}",
        mode
    );
}

#[test]
fn stale_temp_files_are_removed_at_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let mdr_dir = tmp.path().join("mdr");
    std::fs::create_dir_all(&mdr_dir).unwrap();

    let stale = mdr_dir.join("stdin-999999.md");
    std::fs::write(&stale, "# stale").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(48 * 3600))
        .unwrap();

    let fresh = mdr_dir.join("stdin-888888.md");
    std::fs::write(&fresh, "# fresh").unwrap();

    let unrelated = mdr_dir.join("keep-me.md");
    std::fs::write(&unrelated, "# keep").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&unrelated)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(48 * 3600))
        .unwrap();

    let (_, _stderr) = run_piped(tmp.path(), &["-", "-b", "tui"], b"# cleanup\n");

    assert!(
        !stale.exists(),
        "stdin temp files older than a day should be cleaned up at startup"
    );
    assert!(
        fresh.exists(),
        "recent stdin temp files belong to other running instances and must be kept"
    );
    assert!(
        unrelated.exists(),
        "files that are not stdin temp files must never be touched"
    );
}

#[test]
fn temp_dir_occupied_by_a_regular_file_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("mdr"), "not a directory").unwrap();

    let (ok, stderr) = run_piped(tmp.path(), &["-", "-b", "tui"], b"# oops\n");

    assert!(!ok, "mdr should fail when <tmpdir>/mdr is not a directory");
    assert!(
        stderr.contains("temp"),
        "error should mention the temp directory, got: {}",
        stderr
    );
}

#[cfg(unix)]
#[test]
fn temp_dir_symlink_is_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, tmp.path().join("mdr")).unwrap();

    let (ok, _stderr) = run_piped(tmp.path(), &["-", "-b", "tui"], b"# symlink trap\n");

    assert!(!ok, "mdr should refuse a symlinked temp directory");
    assert!(
        stdin_temp_files(&elsewhere).is_empty(),
        "mdr must not write piped content through a symlinked temp directory"
    );
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
