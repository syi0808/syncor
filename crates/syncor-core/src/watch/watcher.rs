use std::path::PathBuf;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// An event emitted by the debounced watcher.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub changed: bool,
}

/// A file-system watcher that debounces rapid change events.
pub struct DebouncedWatcher {
    // Holds the underlying watcher so it is not dropped prematurely.
    _watcher: RecommendedWatcher,
}

impl DebouncedWatcher {
    /// Start watching `path` recursively.
    ///
    /// A single [`WatchEvent`] is sent to `sender` after `debounce_duration`
    /// has elapsed with no further fs events.
    pub fn start(
        path: PathBuf,
        debounce_duration: Duration,
        sender: mpsc::Sender<WatchEvent>,
    ) -> notify::Result<Self> {
        // Internal channel: the notify callback sends `()` each time an fs
        // event arrives; the tokio task drains the channel and debounces.
        let (raw_tx, mut raw_rx) = mpsc::channel::<()>(64);

        let watcher_tx = raw_tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                // Best-effort: ignore send errors (task may have exited).
                let _ = watcher_tx.try_send(());
            }
        })?;

        watcher.watch(&path, RecursiveMode::Recursive)?;

        // Spawn the debounce task.
        tokio::spawn(async move {
            loop {
                // Wait for the first raw event.
                if raw_rx.recv().await.is_none() {
                    // Channel closed; watcher was dropped.
                    break;
                }

                // Drain all events that arrive within the debounce window.
                loop {
                    match tokio::time::timeout(debounce_duration, raw_rx.recv()).await {
                        // Another event arrived before the timeout — reset the window.
                        Ok(Some(())) => continue,
                        // Channel closed.
                        Ok(None) => {
                            // Still emit the event before exiting.
                            let _ = sender.send(WatchEvent { changed: true }).await;
                            return;
                        }
                        // Timeout elapsed with no further events — debounce complete.
                        Err(_elapsed) => break,
                    }
                }

                // Send the debounced event; stop if the receiver is gone.
                if sender.send(WatchEvent { changed: true }).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self { _watcher: watcher })
    }
}
