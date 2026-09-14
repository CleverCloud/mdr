use notify_debouncer_mini::{DebouncedEventKind, new_debouncer};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// Start watching a file for changes with 300ms debounce.
/// Returns a Receiver that gets a () signal on each change.
pub fn watch_file(path: &Path) -> Result<Receiver<()>, Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::channel();
    let path = path.canonicalize()?;
    let watch_path = path.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(300),
        move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            if let Ok(events) = res {
                for event in &events {
                    if event.kind == DebouncedEventKind::Any && event.path == path {
                        let _ = tx.send(());
                        return;
                    }
                }
            }
        },
    )?;

    let parent = watch_path.parent().unwrap_or(&watch_path);
    debouncer
        .watcher()
        .watch(parent, notify::RecursiveMode::NonRecursive)?;

    // Leak the debouncer so it lives for the program duration.
    // Box::leak makes the intent explicit compared to mem::forget.
    let _ = Box::leak(Box::new(debouncer));

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// How long to give the OS to notice. The debounce is 300 ms; the rest is
    /// slack for a loaded machine, and the test returns as soon as the signal
    /// arrives rather than waiting this out.
    const PATIENCE: Duration = Duration::from_secs(10);

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mdr_watcher_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Swallow whatever is already queued, so the next assertion is about the
    /// change it makes rather than about an earlier one.
    fn drain(rx: &Receiver<()>) {
        while rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
    }

    #[test]
    fn an_edit_to_the_watched_file_is_reported() {
        let dir = temp_dir("edit");
        let file = dir.join("doc.md");
        std::fs::write(&file, "# one\n").unwrap();

        let rx = watch_file(&file).expect("the file exists, so watching it must work");
        drain(&rx);

        std::fs::write(&file, "# two\n").unwrap();
        assert!(
            rx.recv_timeout(PATIENCE).is_ok(),
            "editing the watched file should have produced a signal"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_atomic_replacement_is_reported_too() {
        // What editors actually do: write a temporary next to the file, then
        // rename it over the top. The inode changes, so watching the file
        // itself would miss it — which is why the parent directory is watched.
        let dir = temp_dir("atomic");
        let file = dir.join("doc.md");
        std::fs::write(&file, "# one\n").unwrap();

        let rx = watch_file(&file).unwrap();
        drain(&rx);

        let tmp = dir.join("doc.md.new");
        let mut handle = std::fs::File::create(&tmp).unwrap();
        handle.write_all(b"# replaced\n").unwrap();
        handle.sync_all().unwrap();
        drop(handle);
        std::fs::rename(&tmp, &file).unwrap();

        assert!(
            rx.recv_timeout(PATIENCE).is_ok(),
            "replacing the file through a rename should have produced a signal"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_neighbour_in_the_same_directory_is_ignored() {
        // The parent directory is watched, so every file in it produces events;
        // only the one being viewed should wake the backend.
        let dir = temp_dir("neighbour");
        let file = dir.join("doc.md");
        std::fs::write(&file, "# one\n").unwrap();

        let rx = watch_file(&file).unwrap();
        drain(&rx);

        std::fs::write(dir.join("other.md"), "# unrelated\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "unrelated\n").unwrap();

        assert!(
            rx.recv_timeout(Duration::from_secs(2)).is_err(),
            "a change to a neighbouring file should not have woken the viewer"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watching_a_missing_file_is_an_error() {
        let missing = std::env::temp_dir().join("mdr_watcher_no_such_file.md");
        let _ = std::fs::remove_file(&missing);
        assert!(
            watch_file(&missing).is_err(),
            "there is nothing to canonicalise, so this cannot succeed quietly"
        );
    }

    #[test]
    fn the_watcher_outlives_the_call_that_created_it() {
        // `watch_file` leaks its debouncer on purpose; if that ever stopped
        // being true the watcher would be dropped on return and every signal
        // would be lost. This is the only observable consequence.
        let dir = temp_dir("lifetime");
        let file = dir.join("doc.md");
        std::fs::write(&file, "# one\n").unwrap();

        let rx = {
            let rx = watch_file(&file).unwrap();
            drain(&rx);
            rx
        };

        std::fs::write(&file, "# two\n").unwrap();
        assert!(
            rx.recv_timeout(PATIENCE).is_ok(),
            "the watcher must still be alive after `watch_file` returned"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
