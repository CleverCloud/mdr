use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_config(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mdr_test_config_{}.kdl", name));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn default_config_is_valid_kdl_v2() {
        kdl::KdlDocument::parse_v2(DEFAULT_CONFIG)
            .expect("DEFAULT_CONFIG must be valid KDL v2");
    }

    #[test]
    fn load_parses_backend_unquoted() {
        let path = tmp_config("backend", "backend webview\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.backend.as_deref(), Some("webview"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_parses_backend_quoted() {
        let path = tmp_config("backend_quoted", "backend \"egui\"\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.backend.as_deref(), Some("egui"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_parses_verbose_bool() {
        let path = tmp_config("verbose_true", "verbose #true\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.verbose, Some(true));

        let path2 = tmp_config("verbose_false", "verbose #false\n");
        let cfg2 = load(&path2).unwrap();
        assert_eq!(cfg2.verbose, Some(false));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn load_bare_verbose_node_means_true() {
        let path = tmp_config("verbose_bare", "verbose\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.verbose, Some(true));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_returns_defaults_for_missing_file() {
        let path = PathBuf::from("/nonexistent/mdr_no_such_config.kdl");
        let cfg = load(&path).unwrap();
        assert!(cfg.backend.is_none());
        assert!(cfg.verbose.is_none());
    }

    #[test]
    fn write_default_creates_valid_kdl_v2_file() {
        let path = std::env::temp_dir().join("mdr_test_config_write_default.kdl");
        let _ = std::fs::remove_file(&path);
        write_default(&path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        kdl::KdlDocument::parse_v2(&content)
            .expect("written config must be valid KDL v2");
        assert!(content.contains("backend webview"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_default_errors_if_file_exists() {
        let path = tmp_config("write_exists", "// existing\n");
        assert!(write_default(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}

/// Resolved configuration from a KDL v2 config file.
#[derive(Default, Debug)]
pub struct Config {
    pub backend: Option<String>,
    pub verbose: Option<bool>,
}

const DEFAULT_CONFIG: &str = "\
// mdr configuration — https://github.com/CleverCloud/mdr

// Rendering backend: auto, egui, webview, tui
backend webview

// Uncomment to enable verbose logging by default
// verbose #true
";

/// Returns the default config file path: `~/.config/mdr/config.kdl`.
pub fn default_path() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/mdr/config.kdl")
}

/// Write the default config to `path`, creating parent directories as needed.
/// Returns an error if the file already exists.
pub fn write_default(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Err(format!(
            "config file already exists at '{}' — delete it first to reinitialise",
            path.display()
        )
        .into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, DEFAULT_CONFIG)?;
    Ok(())
}

/// Load config from `path`. Returns defaults if the file does not exist.
/// Errors on parse failure.
pub fn load(path: &PathBuf) -> Result<Config, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)?;
    let doc = kdl::KdlDocument::parse_v2(&text)?;
    let mut cfg = Config::default();
    for node in doc.nodes() {
        match node.name().value() {
            "backend" => {
                if let Some(kdl::KdlValue::String(s)) = node.get(0) {
                    cfg.backend = Some(s.clone());
                }
            }
            "verbose" => {
                cfg.verbose = Some(match node.get(0) {
                    None => true, // bare `verbose` node = true
                    Some(kdl::KdlValue::Bool(b)) => *b,
                    _ => true,
                });
            }
            other => eprintln!("mdr: unknown config key '{}'", other),
        }
    }
    Ok(cfg)
}
