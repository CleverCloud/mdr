use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
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
