use std::process::Command;

fn mdr_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove "deps"
    path.push("mdr");
    path
}

#[test]
fn init_creates_config_at_custom_path() {
    let path = std::env::temp_dir().join("mdr_test_init_creates.kdl");
    let _ = std::fs::remove_file(&path);

    let output = Command::new(mdr_bin())
        .args(["--init", "--config"])
        .arg(&path)
        .output()
        .expect("failed to run mdr");

    assert!(output.status.success(), "mdr --init should succeed");
    assert!(path.exists(), "config file should be created");

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("backend webview"), "config should set default backend");

    // File must be valid KDL v2
    kdl::KdlDocument::parse_v2(&content)
        .expect("--init output must be valid KDL v2");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn init_errors_if_config_already_exists() {
    let path = std::env::temp_dir().join("mdr_test_init_exists.kdl");
    std::fs::write(&path, "// existing\n").unwrap();

    let output = Command::new(mdr_bin())
        .args(["--init", "--config"])
        .arg(&path)
        .output()
        .expect("failed to run mdr");

    assert!(!output.status.success(), "mdr --init should fail if config exists");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already exists"),
        "stderr should mention file already exists, got: {}",
        stderr
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn explicit_missing_config_errors() {
    // --config pointing to a nonexistent file (without --init) should error.
    // We pass a markdown file too so that the process reaches config-loading.
    let md = std::env::temp_dir().join("mdr_test_cfg_missing.md");
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
        "should report config file not found, got: {}",
        stderr
    );

    let _ = std::fs::remove_file(&md);
}
