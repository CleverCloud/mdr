use std::process::Command;

/// A directory of this test's own.
///
/// A fixed name under the system temp directory is shared with every other run
/// of the suite: two at once delete each other's fixtures. The `TempDir` also
/// takes the cleanup off each test's hands.
fn sandbox() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp directory")
}

fn mdr_bin() -> std::path::PathBuf {
    // Cargo hands the test the path it built, extension and all; deriving it
    // from `current_exe` guessed wrong on Windows, where it is `mdr.exe`.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_mdr"))
}

#[test]
fn the_default_config_is_created_on_first_run() {
    // `--init` is gone: the file appears by itself, so the settings are
    // discoverable without having to know a flag exists.
    let sandbox = sandbox();
    let home = sandbox.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let expected = home.join(".config").join("mdr").join("config.kdl");

    let md = sandbox.path().join("doc.md");
    std::fs::write(&md, "# test\n").unwrap();

    // The tui backend refuses a non-TTY stdout, so this exits without opening
    // anything — but only after the config has been created on the way in.
    let output = Command::new(mdr_bin())
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("APPDATA")
        .env("USERPROFILE", &home)
        .arg("--backend")
        .arg("tui")
        .arg(&md)
        .output()
        .expect("failed to run mdr");

    assert!(
        expected.exists(),
        "the config should have been created at {}, stderr: {}",
        expected.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let content = std::fs::read_to_string(&expected).unwrap();
    assert!(
        content.contains("backend auto"),
        "the generated config must select a backend every build has, got:\n{content}"
    );
    kdl::KdlDocument::parse_v2(&content).expect("the generated config must be valid KDL v2");
}

#[test]
fn an_existing_config_is_never_overwritten_on_start() {
    let sandbox = sandbox();
    let home = sandbox.path().join("home");
    let dir = home.join(".config").join("mdr");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.kdl");
    std::fs::write(&path, "// mine\nbackend tui\n").unwrap();

    let md = sandbox.path().join("doc.md");
    std::fs::write(&md, "# test\n").unwrap();

    Command::new(mdr_bin())
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("APPDATA")
        .env("USERPROFILE", &home)
        .arg("--backend")
        .arg("tui")
        .arg(&md)
        .output()
        .expect("failed to run mdr");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "// mine\nbackend tui\n",
        "starting mdr must not rewrite a config the user already has"
    );
}

#[test]
fn explicit_missing_config_errors() {
    // --config pointing to a nonexistent file should error rather than create it.
    // We pass a markdown file too so that the process reaches config-loading.
    let sandbox = sandbox();
    let md = sandbox.path().join("doc.md");
    std::fs::write(&md, "# test\n").unwrap();

    let output = Command::new(mdr_bin())
        .arg("--config")
        .arg("/nonexistent/mdr_no_such.kdl")
        .arg(&md)
        .output()
        .expect("failed to run mdr");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "should report config file not found, got: {stderr}"
    );
}

/// A config that exists, in a directory of its own.
fn config_with(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.kdl");
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

#[test]
fn set_default_backend_writes_through_the_command_line() {
    // The unit tests call `set_backend` directly; this is the path a user takes,
    // including the argument parsing and the exit code.
    let (_dir, path) = config_with("// mine\nbackend auto\nverbose #true\n");

    let output = Command::new(mdr_bin())
        .arg("-s")
        .arg("tui")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("failed to run mdr");

    assert!(
        output.status.success(),
        "mdr -s should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("backend tui"), "got:\n{after}");
    assert!(
        after.contains("// mine") && after.contains("verbose #true"),
        "the rest of the file must survive, got:\n{after}"
    );
}

#[test]
fn set_default_backend_refuses_a_name_that_is_not_a_backend() {
    let (_dir, path) = config_with("backend auto\n");

    let output = Command::new(mdr_bin())
        .arg("-s")
        .arg("webview") // the name before 0.6; not a backend any more
        .arg("-c")
        .arg(&path)
        .output()
        .expect("failed to run mdr");

    assert!(
        !output.status.success(),
        "an unknown backend must not succeed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("auto, gui, tui, web"),
        "the error should list what is accepted, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "backend auto\n",
        "a refused value must leave the file alone"
    );
}

#[test]
fn set_default_backend_will_not_create_the_file_it_was_pointed_at() {
    // Same rule as reading: an explicit `--config` path that does not exist is
    // a typo, not a request to create one.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.kdl");

    let output = Command::new(mdr_bin())
        .arg("-s")
        .arg("tui")
        .arg("-c")
        .arg(&missing)
        .output()
        .expect("failed to run mdr");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not found"),
        "should report the missing file"
    );
    assert!(!missing.exists(), "nothing should have been created");
}

#[test]
fn the_command_line_backend_beats_the_config_file() {
    // `--backend` is typed now; the file was written some time ago.
    let (dir, path) = config_with("backend web\n");
    let md = dir.path().join("doc.md");
    std::fs::write(&md, "# test\n").unwrap();

    let output = Command::new(mdr_bin())
        .arg("-c")
        .arg(&path)
        .arg("-b")
        .arg("tui")
        .arg(&md)
        .output()
        .expect("failed to run mdr");

    // The tui backend refuses a non-TTY stdout, which is the proof it was the
    // one chosen: had the file won, this would have tried to open a window.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("tui backend requires a terminal"),
        "the command line should have won, got: {stderr}"
    );

    let _ = std::fs::remove_file(&md);
}
