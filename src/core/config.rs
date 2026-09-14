use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_config(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mdr_test_config_{name}.kdl"));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn default_config_is_valid_kdl_v2() {
        kdl::KdlDocument::parse_v2(DEFAULT_CONFIG).expect("DEFAULT_CONFIG must be valid KDL v2");
    }

    #[test]
    fn load_parses_backend_unquoted() {
        let path = tmp_config("backend", "backend web\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.backend.as_deref(), Some("web"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_parses_backend_quoted() {
        let path = tmp_config("backend_quoted", "backend \"gui\"\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.backend.as_deref(), Some("gui"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn backend_names_are_the_documented_ones() {
        for name in BACKENDS {
            assert!(is_valid_backend(name), "{name} should be accepted");
        }
        // Renamed in this release: the old names must not pass as current ones,
        // they go through `renamed_backend` instead.
        for name in ["", "egui", "webview", "nonsense", "GUI"] {
            assert!(!is_valid_backend(name), "{name} should be rejected");
        }
    }

    #[test]
    fn every_renamed_backend_maps_onto_a_current_one() {
        for (old, current) in RENAMED_BACKENDS {
            assert_eq!(renamed_backend(old), Some(*current));
            assert!(
                is_valid_backend(current),
                "'{old}' maps to '{current}', which must itself be a backend"
            );
            assert!(
                !is_valid_backend(old),
                "'{old}' must no longer be accepted as a current name"
            );
        }
        assert_eq!(
            renamed_backend("gui"),
            None,
            "a current name is not renamed"
        );
        assert_eq!(renamed_backend("nonsense"), None);
    }

    #[test]
    fn a_config_written_before_the_rename_is_corrected_in_place() {
        // Someone who set `backend egui` before 0.6 must not be dropped onto
        // auto-detection, and must not have to edit the file by hand either:
        // the run that reads it is the run that fixes it.
        let path = tmp_config(
            "backend_legacy",
            "// keep me\nbackend \"egui\"\n\n// and me\nverbose #true\n",
        );
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.backend.as_deref(), Some("gui"));
        assert_eq!(cfg.verbose, Some(true), "the rest of the file must survive");

        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(
            rewritten.contains(r#"backend "gui""#),
            "the file must carry the current name, quoted as the user had it, got:\n{rewritten}"
        );
        assert!(
            !rewritten.contains("egui"),
            "the old name must be gone, got:\n{rewritten}"
        );
        assert!(
            rewritten.contains("// keep me") && rewritten.contains("// and me"),
            "comments must survive the rewrite, got:\n{rewritten}"
        );
        assert!(
            rewritten.contains("verbose #true"),
            "other settings must survive the rewrite, got:\n{rewritten}"
        );
        kdl::KdlDocument::parse_v2(&rewritten).expect("the rewrite must stay valid KDL v2");

        // Reading it again is now an ordinary load, with nothing to migrate.
        assert_eq!(load(&path).unwrap().backend.as_deref(), Some("gui"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_read_only_config_is_not_replaced() {
        // A rename only needs the directory to be writable, so without an
        // explicit check the migration would overwrite a file the user locked —
        // something the plain `fs::write` it replaced could never have done.
        let path = tmp_config("readonly", "backend auto\n");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        let err = publish_atomically(&path, "backend tui\n").expect_err("must refuse");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "backend auto\n",
            "the file must be untouched"
        );

        // Put the file back before removing it: a read-only file cannot be
        // deleted on Windows. `set_readonly(false)` would make it world-writable
        // on Unix, so restore an explicit mode there instead.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        {
            // The lint warns that this makes a file world-writable — which it
            // does, on Unix. That is exactly why this branch is the one Unix
            // does not take.
            #[allow(clippy::permissions_set_readonly_false)]
            {
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_is_not_replaced() {
        // A rename would swap the link for a regular file, quietly undoing a
        // deliberate arrangement. Every path that rewrites the config shares
        // this guard, `-s` included.
        // A directory of its own: a fixed name under the system temp directory
        // is shared with every other run of the suite, and two at once would
        // delete each other's fixture.
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let real = dir.join("real.kdl");
        std::fs::write(&real, "backend auto\n").unwrap();
        let link = dir.join("config.kdl");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        {
            let err = publish_atomically(&link, "backend tui\n").expect_err("must refuse");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "the link must survive"
            );
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "backend auto\n");

            // `-s` goes through the same guard.
            assert!(set_backend(&link, "web").is_err());
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "backend auto\n");
        }
    }

    #[test]
    fn publishing_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let path = dir.join("config.kdl");
        std::fs::write(&path, "backend auto\n").unwrap();

        publish_atomically(&path, "backend web\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "backend web\n");

        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "config.kdl")
            .collect();
        assert!(
            leftovers.is_empty(),
            "the temporary must be gone, found: {leftovers:?}"
        );
    }

    #[test]
    fn set_backend_replaces_the_value_and_keeps_the_rest_of_the_file() {
        let path = tmp_config(
            "set_backend",
            "// keep me\nbackend auto\n\n// and me\nverbose #true\n",
        );
        set_backend(&path, "web").unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("backend web"), "got:\n{after}");
        assert!(
            after.contains("// keep me") && after.contains("// and me"),
            "comments must survive, got:\n{after}"
        );
        assert!(
            after.contains("verbose #true"),
            "other settings must survive, got:\n{after}"
        );
        assert_eq!(load(&path).unwrap().backend.as_deref(), Some("web"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn set_backend_changes_the_entry_that_actually_wins() {
        // `load` keeps the last `backend` node, so changing the first would
        // report success and leave the effective default alone.
        let path = tmp_config("set_backend_multi", "backend auto\nbackend tui\n");
        set_backend(&path, "web").unwrap();

        assert_eq!(
            load(&path).unwrap().backend.as_deref(),
            Some("web"),
            "the value mdr will actually use must be the one that changed"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn set_backend_adds_the_entry_when_the_file_has_none() {
        let path = tmp_config("set_backend_absent", "verbose #true\n");
        set_backend(&path, "tui").unwrap();

        let cfg = load(&path).unwrap();
        assert_eq!(cfg.backend.as_deref(), Some("tui"));
        assert_eq!(cfg.verbose, Some(true), "other settings must be kept");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_property_on_the_backend_node_survives_the_correction() {
        // "corriger le nom" is not a licence to drop the rest of the line.
        let path = tmp_config("backend_props", "backend \"egui\" extra=1\n");
        assert_eq!(load(&path).unwrap().backend.as_deref(), Some("gui"));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("gui"),
            "the name must be corrected, got: {after}"
        );
        assert!(
            after.contains("extra=1"),
            "the property must survive, got: {after}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_legacy_backend_node_is_corrected_not_just_the_first() {
        // The value that counts is the last one; correcting only the first node
        // would leave a legacy name behind and warn again on every start.
        let path = tmp_config("backend_multi", "backend \"egui\"\nbackend \"webview\"\n");
        assert_eq!(
            load(&path).unwrap().backend.as_deref(),
            Some("web"),
            "the last entry still wins"
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("egui") && !after.contains("webview"),
            "no legacy name may be left behind, got: {after}"
        );

        // Second run: nothing left to migrate, so nothing is rewritten.
        assert_eq!(load(&path).unwrap().backend.as_deref(), Some("web"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_other_old_backend_name_is_corrected_too() {
        // Written bare here, so it must come back bare: the correction changes
        // the name, not how the user chose to write it.
        let path = tmp_config("backend_legacy_webview", "backend webview\n");
        assert_eq!(load(&path).unwrap().backend.as_deref(), Some("web"));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("backend web") && !after.contains('"'),
            "a bare value must stay bare, got: {after}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_backend_is_dropped_rather_than_stored() {
        // It used to reach the dispatch table and hit `unreachable!()`, aborting
        // the process over a stale line in a file the user was not editing.
        let path = tmp_config("backend_bogus", "backend \"nonsense\"\nverbose #true\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.backend, None, "the unusable name must not be kept");
        assert_eq!(
            cfg.verbose,
            Some(true),
            "the rest of the file must survive an unusable backend"
        );
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
    fn load_parses_offline_bool() {
        let path = tmp_config("offline_true", "offline #true\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.offline, Some(true));

        let path2 = tmp_config("offline_false", "offline #false\n");
        let cfg2 = load(&path2).unwrap();
        assert_eq!(cfg2.offline, Some(false));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn load_bare_offline_node_means_true() {
        let path = tmp_config("offline_bare", "offline\n");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.offline, Some(true));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_parses_theme() {
        let path = tmp_config("theme_light", "theme \"light\"\n");
        assert_eq!(load(&path).unwrap().theme.as_deref(), Some("light"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_theme_is_refused_rather_than_stored() {
        let path = tmp_config("theme_bogus", "theme \"neon\"\n");
        assert_eq!(load(&path).unwrap().theme, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_returns_defaults_for_missing_file() {
        let path = PathBuf::from("/nonexistent/mdr_no_such_config.kdl");
        let cfg = load(&path).unwrap();
        assert!(cfg.backend.is_none());
        assert!(cfg.verbose.is_none());
        assert!(cfg.offline.is_none());
        assert!(cfg.theme.is_none());
    }

    #[test]
    fn ensure_exists_writes_a_valid_kdl_v2_file() {
        // A directory of its own: a fixed name under the system temp directory
        // is shared with every other run of the suite, and two at once would
        // delete each other's fixture.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        assert!(ensure_exists(&path).unwrap(), "the first call writes it");
        let content = std::fs::read_to_string(&path).unwrap();
        kdl::KdlDocument::parse_v2(&content).expect("written config must be valid KDL v2");
        assert!(content.contains("backend auto"));
        assert!(content.contains("offline"));
        assert!(content.contains("theme"));
    }

    #[test]
    fn ensure_exists_creates_once_and_then_leaves_the_file_alone() {
        // It runs on every start, so it must never overwrite what the user wrote.
        let parent = tempfile::tempdir().unwrap();
        // Not the temp directory itself: the point is that `ensure_exists`
        // creates the directory it was pointed at.
        let path = parent.path().join("mdr").join("config.kdl");

        assert!(ensure_exists(&path).unwrap(), "first call writes the file");
        assert!(path.exists());
        std::fs::write(&path, "backend tui\n").unwrap();

        assert!(!ensure_exists(&path).unwrap(), "second call is a no-op");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "backend tui\n");
    }

    // --- config path resolution ---

    fn resolve_with(vars: &[(&str, &str)], existing: &[&str], windows: bool) -> Resolution {
        let vars: Vec<(String, std::ffi::OsString)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), std::ffi::OsString::from(v)))
            .collect();
        let existing: Vec<PathBuf> = existing.iter().map(PathBuf::from).collect();
        resolve(
            |key| {
                vars.iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.clone())
            },
            |path| existing.iter().any(|candidate| candidate == path),
            windows,
        )
    }

    fn config_in(dir: &str) -> PathBuf {
        PathBuf::from(dir).join("mdr").join(FILE_NAME)
    }

    fn dotconfig_in(home: &str) -> PathBuf {
        PathBuf::from(home)
            .join(".config")
            .join("mdr")
            .join(FILE_NAME)
    }

    /// An absolute path for the host running the tests.
    ///
    /// `resolve` asks `Path::is_absolute`, whose answer comes from the real OS
    /// and not from the `windows` flag the test passes in: `/home/dev/cfg` is
    /// not absolute on Windows, so hard-coding it would make this pass on Unix
    /// and fail there.
    fn absolute(tail: &str) -> String {
        if cfg!(windows) {
            format!("C:\\{tail}")
        } else {
            format!("/{tail}")
        }
    }

    #[test]
    fn xdg_config_home_wins_over_the_home_directory() {
        let xdg = absolute("home/dev/cfg");
        let r = resolve_with(
            &[("HOME", "/home/dev"), ("XDG_CONFIG_HOME", &xdg)],
            &[],
            false,
        );
        assert_eq!(r.path, config_in(&xdg));
        assert!(r.warning.is_none());
    }

    #[test]
    fn home_is_used_when_xdg_is_not_set() {
        let r = resolve_with(&[("HOME", "/home/dev")], &[], false);
        assert_eq!(r.path, dotconfig_in("/home/dev"));
        assert!(r.warning.is_none());
    }

    #[test]
    fn an_existing_dotconfig_file_keeps_priority_over_xdg() {
        // Someone who already has a config must not lose it the day they set
        // XDG_CONFIG_HOME.
        let legacy = dotconfig_in("/home/dev");
        let r = resolve_with(
            &[("HOME", "/home/dev"), ("XDG_CONFIG_HOME", "/home/dev/cfg")],
            &[legacy.to_str().unwrap()],
            false,
        );
        assert_eq!(r.path, legacy);
    }

    #[test]
    fn windows_prefers_userprofile_then_appdata() {
        // Git Bash sets HOME to a POSIX path a native binary cannot resolve, so
        // USERPROFILE comes first on Windows.
        let r = resolve_with(
            &[
                ("HOME", "/c/Users/dev"),
                ("USERPROFILE", "C:\\Users\\dev"),
                ("APPDATA", "C:\\Users\\dev\\AppData\\Roaming"),
            ],
            &[],
            true,
        );
        assert_eq!(r.path, config_in("C:\\Users\\dev\\AppData\\Roaming"));

        // Without APPDATA it falls back to the profile directory.
        let r = resolve_with(&[("USERPROFILE", "C:\\Users\\dev")], &[], true);
        assert_eq!(r.path, dotconfig_in("C:\\Users\\dev"));
    }

    #[test]
    fn a_relative_xdg_config_home_is_ignored() {
        // The spec calls a relative value invalid, and mdr writes the file, so
        // honouring it would drop a config into the current directory.
        let r = resolve_with(
            &[("HOME", "/home/dev"), ("XDG_CONFIG_HOME", "cfg")],
            &[],
            false,
        );
        assert_eq!(r.path, dotconfig_in("/home/dev"));
        assert!(r.warning.is_none(), "the home directory was still found");
    }

    #[test]
    fn empty_variables_are_ignored() {
        let r = resolve_with(
            &[("HOME", "/home/dev"), ("XDG_CONFIG_HOME", "")],
            &[],
            false,
        );
        assert_eq!(r.path, dotconfig_in("/home/dev"));
    }

    #[test]
    fn without_a_home_it_warns_and_falls_back_to_the_current_directory() {
        let r = resolve_with(&[], &[], false);
        assert_eq!(r.path, dotconfig_in("."));
        let warning = r
            .warning
            .expect("a fallback to the current directory must warn");
        assert!(
            warning.contains("HOME"),
            "the warning should name the variables, got: {warning}"
        );
    }
}

/// Backend names accepted on the command line and in the config file.
///
/// One list for both, so a name can never be valid in one place and unknown in
/// the other. It deliberately does not depend on the compiled features: a known
/// but absent backend keeps the explicit "not compiled" error.
pub const BACKENDS: &[&str] = &["auto", "gui", "tui", "web"];

/// The names the backends went by up to 0.5.1, and what each is called now.
///
/// A config file is not what the user is editing when mdr starts, so an old
/// name there is rewritten to the current one in place rather than refused —
/// a file mdr has read and been able to write comes out carrying the current
/// name. That is what makes removing this table cheap later — but not free: a
/// config never opened, or one mdr could not write to, still holds the old
/// name. The command line has no such mapping: it is retyped every run, so an
/// old name is simply not a backend any more.
pub const RENAMED_BACKENDS: &[(&str, &str)] = &[("egui", "gui"), ("webview", "web")];

/// Whether `name` is one of [`BACKENDS`].
pub fn is_valid_backend(name: &str) -> bool {
    BACKENDS.contains(&name)
}

/// The current name of a backend that has been renamed, if `name` is an old one.
pub fn renamed_backend(name: &str) -> Option<&'static str> {
    RENAMED_BACKENDS
        .iter()
        .find(|(old, _)| *old == name)
        .map(|(_, new)| *new)
}

/// Resolved configuration from a KDL v2 config file.
#[derive(Default, Debug)]
pub struct Config {
    pub backend: Option<String>,
    pub verbose: Option<bool>,
    pub offline: Option<bool>,
    /// Colour scheme to assume: `auto`, `dark` or `light`.
    pub theme: Option<String>,
}

const DEFAULT_CONFIG: &str = "\
// mdr configuration — https://github.com/CleverCloud/mdr

// Rendering backend: auto, gui, tui, web
backend auto

// Uncomment to enable verbose logging by default
// verbose #true

// Uncomment to never access the network (remote images are not downloaded)
// offline #true

// Colour scheme the terminal backend assumes for syntax highlighting:
// auto (read COLORFGBG, fall back to dark), dark, or light
// theme \"auto\"
";

const FILE_NAME: &str = "config.kdl";

/// A resolved config location, plus what to tell the user if mdr had to guess.
struct Resolution {
    path: PathBuf,
    warning: Option<String>,
}

/// Resolve the config path from a set of environment variables.
///
/// The lookups are injected so the platform rules can be tested without touching
/// the process environment. Order:
///
/// 1. An existing `<home>/.config/mdr/config.kdl`, so a machine that already has
///    a config keeps using it whatever the environment says now.
/// 2. `$XDG_CONFIG_HOME/mdr/config.kdl` — the XDG Base Directory spec, which is
///    what Linux and BSD users expect when they move their config directory.
/// 3. `%APPDATA%\mdr\config.kdl` on Windows.
/// 4. `<home>/.config/mdr/config.kdl`, `<home>` being `%USERPROFILE%` on Windows
///    (`HOME` is set to a POSIX path by Git Bash and friends, which a native
///    binary cannot resolve) and `$HOME` everywhere else.
/// 5. `./.config/mdr/config.kdl` as a last resort, with a warning: nothing named
///    a home directory, so the file lands wherever mdr was started from.
fn resolve(
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
    exists: impl Fn(&Path) -> bool,
    windows: bool,
) -> Resolution {
    fn non_empty(value: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
        value.filter(|v| !v.is_empty())
    }

    let home = if windows {
        non_empty(env("USERPROFILE")).or_else(|| non_empty(env("HOME")))
    } else {
        non_empty(env("HOME")).or_else(|| non_empty(env("USERPROFILE")))
    }
    .map(PathBuf::from);

    let dotconfig = home.as_ref().map(|home| home.join(".config"));

    if let Some(legacy) = dotconfig
        .as_ref()
        .map(|dir| dir.join("mdr").join(FILE_NAME))
        && exists(&legacy)
    {
        return Resolution {
            path: legacy,
            warning: None,
        };
    }

    // The XDG spec says a relative `XDG_CONFIG_HOME` is invalid and must be
    // ignored. It matters more than pedantry here: mdr creates the file, so
    // honouring a relative value would write a config into the current
    // directory instead of the user's home.
    if let Some(xdg) = non_empty(env("XDG_CONFIG_HOME")).map(PathBuf::from) {
        if xdg.is_absolute() {
            return Resolution {
                path: xdg.join("mdr").join(FILE_NAME),
                warning: None,
            };
        }
        eprintln!(
            "mdr: warning: ignoring XDG_CONFIG_HOME '{}', which must be an absolute path",
            xdg.display()
        );
    }

    if windows && let Some(appdata) = non_empty(env("APPDATA")) {
        return Resolution {
            path: PathBuf::from(appdata).join("mdr").join(FILE_NAME),
            warning: None,
        };
    }

    if let Some(dir) = dotconfig {
        return Resolution {
            path: dir.join("mdr").join(FILE_NAME),
            warning: None,
        };
    }

    let fallback = PathBuf::from(".")
        .join(".config")
        .join("mdr")
        .join(FILE_NAME);
    let vars = if windows {
        "USERPROFILE, HOME, XDG_CONFIG_HOME or APPDATA"
    } else {
        "HOME or XDG_CONFIG_HOME"
    };
    Resolution {
        warning: Some(format!(
            "no home directory found ({vars} unset), falling back to '{}'",
            fallback.display()
        )),
        path: fallback,
    }
}

/// The config file path for this machine, and whether mdr is confident enough
/// in it to create a file there.
///
/// The flag is `false` only for the last-resort path under the current
/// directory: nothing named a home, so mdr will read a config that happens to
/// be there but will not drop a `.config/` into whatever directory it was
/// started from — a repository being browsed, most likely.
pub fn default_location() -> (PathBuf, bool) {
    let resolution = resolve(
        |key| std::env::var_os(key),
        std::path::Path::exists,
        cfg!(windows),
    );
    let confident = resolution.warning.is_none();
    if let Some(warning) = resolution.warning {
        eprintln!("mdr: warning: {warning}");
    }
    (resolution.path, confident)
}

/// Create the config file with mdr's defaults if it is not there yet, creating
/// parent directories as needed. Returns `Ok(true)` when a file was written.
///
/// Replaces the old `--init` flag: a file the program can write for itself is
/// not worth a command line option, and its absence was the only thing keeping
/// new users from discovering that mdr is configurable at all.
pub fn ensure_exists(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // `exists()` then `write` would be a window, not a check: two mdr starting
    // together could both see no file and the second would truncate the first's.
    // Exclusive creation makes "already there" an outcome rather than a race.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(DEFAULT_CONFIG.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Replace a node's first positional argument, leaving everything else alone.
///
/// Properties, any further arguments and the formatting around them are the
/// user's. And `set_value` on its own changes nothing on the way out: a parsed
/// entry keeps the literal text it came from (`KdlEntryFormat::value_repr`) and
/// prints that, so the representation has to be rewritten too — keeping the
/// quoting the user chose, so a quoted value stays quoted and a bare one bare.
fn set_first_argument(node: &mut kdl::KdlNode, value: &str) {
    let Some(entry) = node.entries_mut().iter_mut().find(|e| e.name().is_none()) else {
        return;
    };
    entry.set_value(kdl::KdlValue::String(value.to_string()));
    if let Some(format) = entry.format_mut() {
        format.value_repr = if format.value_repr.trim_start().starts_with('"') {
            format!("\"{value}\"")
        } else {
            value.to_string()
        };
    }
}

/// Write `backend` into the `backend` entry of the config at `path`.
///
/// Everything else in the file is preserved: the document is parsed and only
/// that one value is replaced, so comments and other settings survive. A file
/// without a `backend` node gains one at the end.
pub fn set_backend(path: &Path, backend: &str) -> Result<(), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut doc = kdl::KdlDocument::parse_v2(&text)?;

    // The LAST `backend` node is the one `load` keeps, so that is the one to
    // change: `doc.get_mut` returns the first, and writing to it would report
    // success while leaving the effective default untouched.
    let target = doc
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.name().value() == "backend" && node.entries().iter().any(|e| e.name().is_none())
        })
        .map(|(index, _)| index)
        .next_back();

    match target.and_then(|index| doc.nodes_mut().get_mut(index)) {
        Some(node) => set_first_argument(node, backend),
        // Either no `backend` node at all, or none with a value to replace.
        None => {
            let mut node = kdl::KdlNode::new("backend");
            node.push(kdl::KdlValue::String(backend.to_string()));
            doc.nodes_mut().push(node);
        }
    }

    publish_atomically(path, &doc.to_string())?;
    Ok(())
}

/// Rewrite the old backend names in `path` to the current ones.
///
/// The point is that the migration mostly removes itself: a file mdr has read
/// and been able to write no longer carries a name from before the rename. It
/// is best-effort — the value is already mapped in memory by the time this
/// runs, so nothing here can make the run fail — which is also why deleting
/// this table later is not free: a config that was never opened, or that mdr
/// could not write to, still holds the old name.
///
/// Whoever removes it should keep `renamed_backend` reporting a clear message
/// for those names, rather than letting them fall through as unknown.
fn migrate_backend_names(
    path: &Path,
    original: &str,
    doc: &mut kdl::KdlDocument,
    migrations: &[(usize, String, &'static str)],
) {
    // A symlink is a deliberate arrangement: replacing it with a regular file
    // through a rename would quietly undo it, so say so and leave it alone.
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        report_unmigrated(path, migrations, "it is a symlink");
        return;
    }

    for (index, _, current) in migrations {
        if let Some(node) = doc.nodes_mut().get_mut(*index) {
            set_first_argument(node, current);
        }
    }

    // Someone may have edited the file between the read and now; publishing the
    // parse of a stale read would throw their edit away.
    match std::fs::read_to_string(path) {
        Ok(current_text) if current_text == original => {}
        Ok(_) => {
            report_unmigrated(path, migrations, "it changed while mdr was starting");
            return;
        }
        Err(e) => {
            report_unmigrated(path, migrations, &format!("it could not be re-read: {e}"));
            return;
        }
    }

    if let Err(e) = publish_atomically(path, &doc.to_string()) {
        report_unmigrated(path, migrations, &format!("it could not be written: {e}"));
        return;
    }

    for (_, old, current) in migrations {
        eprintln!(
            "mdr: backend '{old}' is now called '{current}'; updated {}",
            path.display()
        );
    }
}

/// Say that the names were mapped for this run but the file was left as it is.
fn report_unmigrated(path: &Path, migrations: &[(usize, String, &'static str)], why: &str) {
    for (_, old, current) in migrations {
        eprintln!(
            "mdr: backend '{old}' is now called '{current}'; using it, but {} was left \
             unchanged because {why}",
            path.display()
        );
    }
}

/// Replace `path`'s contents in one step, so a crash cannot leave the config
/// half-written. The temporary lands in the same directory, since a rename only
/// counts as atomic within a filesystem.
/// A temporary file that removes itself unless it is handed off.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl TempFile {
    /// Create a uniquely named file in `dir`, failing rather than reusing one.
    ///
    /// The name has to be unique and the creation exclusive: a fixed
    /// `config.kdl.new` would collide with a file already sitting there, and two
    /// mdr migrating the same config at once would write through each other.
    ///
    /// `permissions` are applied before the caller can write a single byte, so
    /// the contents of a restricted config never sit in a laxer file — not even
    /// for the width of a write.
    fn create_in(
        dir: &Path,
        permissions: &std::fs::Permissions,
    ) -> std::io::Result<(Self, std::fs::File)> {
        let pid = std::process::id();
        for attempt in 0..64u32 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let path = dir.join(format!(".mdr-config-{pid}-{nanos}-{attempt}.tmp"));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            // Created private, not widened later: `set_permissions` after the
            // fact does not revoke a descriptor another process already opened.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    let tmp = Self { path, armed: true };
                    // Before any content. A failure drops `tmp`, which removes
                    // the empty file, and leaves the original untouched.
                    file.set_permissions(permissions.clone())?;
                    return Ok((tmp, file));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not find a free temporary name next to the config file",
        ))
    }

    /// Give up ownership: the file is now somewhere else, under another name.
    fn handed_off(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Replace `path`'s contents in one step, so a crash cannot leave the config
/// half-written.
///
/// What this does and does not promise:
///
/// - The replacement is atomic *for readers of `path`*: they see the old file
///   or the new one, never a truncated one. The temporary lands in the same
///   directory, since a rename is only atomic within a filesystem.
/// - It is **not** a lock. A writer that changes the file between the caller's
///   read and this rename loses its change; narrowing that window is all the
///   re-read in [`migrate_backend_names`] does.
/// - It refuses a read-only file. `fs::write` would have failed on one, whereas
///   a rename only needs the *directory* to be writable — so without this check
///   the migration would quietly overwrite a config the user had locked.
/// - It carries the original's **mode and read-only bit** onto the replacement,
///   and applies them before any content is written. It does not preserve what
///   `std::fs::Permissions` cannot express — POSIX ACLs, ownership, extended
///   attributes — so this is mode preservation, not a general guarantee that
///   the file's authorisations survive.
/// - It refuses a symlink, since a rename would replace the link itself.
fn publish_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;

    // A symlink is a deliberate arrangement, and a rename would replace it with
    // a regular file. This guard lives here rather than in one caller so that
    // every path that rewrites the config obeys the same policy.
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the config file is a symlink",
        ));
    }

    let meta = std::fs::metadata(path)?;
    if meta.permissions().readonly() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the config file is read-only",
        ));
    }

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let (tmp, mut file) = TempFile::create_in(dir, &meta.permissions())?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);

    std::fs::rename(&tmp.path, path)?;
    tmp.handed_off();
    Ok(())
}

/// Load config from `path`. Returns defaults if the file does not exist.
/// Errors on parse failure.
pub fn load(path: &PathBuf) -> Result<Config, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)?;
    let mut doc = kdl::KdlDocument::parse_v2(&text)?;
    let mut cfg = Config::default();
    // Which `backend` nodes carry a name from before the rename, by index, so
    // the rewrite below touches those and not merely the first node of that
    // name: a file may hold several, and the last one is the one that counts.
    let mut migrations: Vec<(usize, String, &'static str)> = Vec::new();
    for (index, node) in doc.nodes().iter().enumerate() {
        match node.name().value() {
            "backend" => {
                if let Some(kdl::KdlValue::String(s)) = node.get(0) {
                    // A name this binary does not know used to travel all the
                    // way to the dispatch table and hit `unreachable!()`. The
                    // file is not what the user is editing right now, so an
                    // unusable value is reported and this entry ignored rather
                    // than fatal — whatever a previous `backend` node set is
                    // kept, and `--backend` still wins over the file either way.
                    if is_valid_backend(s) {
                        cfg.backend = Some(s.clone());
                    } else if let Some(current) = renamed_backend(s) {
                        cfg.backend = Some(current.to_string());
                        migrations.push((index, s.clone(), current));
                    } else {
                        eprintln!(
                            "mdr: unknown backend '{s}' in {}, ignoring it (expected one of: {})",
                            path.display(),
                            BACKENDS.join(", ")
                        );
                    }
                }
            }
            "verbose" => {
                cfg.verbose = Some(match node.get(0) {
                    None => true, // bare `verbose` node = true
                    Some(kdl::KdlValue::Bool(b)) => *b,
                    _ => true,
                });
            }
            "theme" => {
                if let Some(kdl::KdlValue::String(s)) = node.get(0) {
                    if crate::core::Theme::parse(s).is_some() {
                        cfg.theme = Some(s.clone());
                    } else {
                        eprintln!("mdr: unknown theme '{s}', expected 'auto', 'dark' or 'light'");
                    }
                }
            }
            "offline" => {
                cfg.offline = Some(match node.get(0) {
                    None => true, // bare `offline` node = true
                    Some(kdl::KdlValue::Bool(b)) => *b,
                    _ => true,
                });
            }
            other => eprintln!("mdr: unknown config key '{other}'"),
        }
    }

    if !migrations.is_empty() {
        migrate_backend_names(path, &text, &mut doc, &migrations);
    }

    Ok(cfg)
}
