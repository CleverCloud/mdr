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
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Instant;

    /// How long to give the OS to notice. The debounce is 300 ms; the rest is
    /// slack for a loaded machine, and each wait returns as soon as the signal
    /// arrives rather than sitting this out.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// Swallow whatever is already queued, so the next assertion is about the
    /// change it makes and not an earlier one. Bounded: a directory that keeps
    /// producing events must not park the test here for ever.
    fn drain(rx: &Receiver<()>) {
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
    }

    #[test]
    fn an_edit_to_the_watched_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        std::fs::write(&file, "# one\n").unwrap();

        // Nothing below arrives unless the watcher is still alive after
        // `watch_file` returned, so this covers that too.
        let rx = watch_file(&file).expect("the file exists, so watching it must work");
        drain(&rx);

        std::fs::write(&file, "# two\n").unwrap();
        assert_eq!(
            rx.recv_timeout(PATIENCE),
            Ok(()),
            "editing the watched file should have produced a signal"
        );
    }

    #[test]
    fn watching_survives_an_atomic_replacement() {
        // What editors actually do: write a temporary next to the file, then
        // rename it over the top. The inode changes, so watching the file
        // itself would miss it — which is why the parent directory is watched.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        std::fs::write(&file, "# one\n").unwrap();

        let rx = watch_file(&file).unwrap();
        drain(&rx);

        let tmp = dir.path().join("doc.md.new");
        let mut handle = std::fs::File::create(&tmp).unwrap();
        handle.write_all(b"# replaced\n").unwrap();
        handle.sync_all().unwrap();
        drop(handle);
        std::fs::rename(&tmp, &file).unwrap();

        assert_eq!(
            rx.recv_timeout(PATIENCE),
            Ok(()),
            "replacing the file through a rename should have produced a signal"
        );

        // And the interesting part: reloading has to keep working afterwards,
        // on the file that now sits behind a different inode.
        drain(&rx);
        std::fs::write(&file, "# edited after the rename\n").unwrap();
        assert_eq!(
            rx.recv_timeout(PATIENCE),
            Ok(()),
            "the watch should have survived the replacement"
        );
    }

    #[test]
    fn a_neighbour_in_the_same_directory_is_ignored() {
        // The parent directory is watched, so every file in it produces events;
        // only the one being viewed should wake the backend.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        std::fs::write(&file, "# one\n").unwrap();

        let rx = watch_file(&file).unwrap();
        drain(&rx);

        std::fs::write(dir.path().join("other.md"), "# unrelated\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "unrelated\n").unwrap();

        // Specifically a timeout: `is_err()` would also accept `Disconnected`,
        // which is what a dead watcher looks like — and a dead watcher ignores
        // everything, including the file we care about.
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)),
            Err(RecvTimeoutError::Timeout),
            "a change to a neighbouring file should not have woken the viewer"
        );

        // So prove it is still listening.
        std::fs::write(&file, "# two\n").unwrap();
        assert_eq!(
            rx.recv_timeout(PATIENCE),
            Ok(()),
            "the watcher must still be alive after ignoring the neighbours"
        );
    }

    #[test]
    fn watching_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            watch_file(&dir.path().join("no-such-file.md")).is_err(),
            "there is nothing to canonicalise, so this cannot succeed quietly"
        );
    }
}
