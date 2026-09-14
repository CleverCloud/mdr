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
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove "deps"
    path.push("mdr");
    path
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
