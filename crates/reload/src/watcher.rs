//! File watching: trigger a reload event after debounce.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, warn};

/// Watch event.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A `.map` file was modified and needs a reload.
    MapChanged(PathBuf),
    /// The watcher errored.
    Error(String),
}

/// `.map` file watcher (with debounce).
pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<WatchEvent>,
    debounce: Duration,
    /// Time of the last event (used for debounce).
    pending: Option<(PathBuf, Instant)>,
}

/// Watcher error.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("failed to create watcher: {0}")]
    Notify(#[from] notify::Error),
    #[error("failed to watch path {path}: {source}")]
    Watch {
        path: PathBuf,
        source: notify::Error,
    },
}

impl FileWatcher {
    /// Create a watcher that watches `.map` files under `dir`.
    ///
    /// `debounce_ms` defaults to 100ms.
    pub fn new(dir: &Path, debounce_ms: u64) -> Result<Self, WatcherError> {
        let (tx, rx) = mpsc::channel();

        let event_tx = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(event) => {
                    if Self::is_map_write(&event) {
                        for path in &event.paths {
                            let _ = event_tx.send(WatchEvent::MapChanged(path.clone()));
                        }
                    }
                }
                Err(e) => {
                    let _ = event_tx.send(WatchEvent::Error(e.to_string()));
                }
            })?;

        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(|source| WatcherError::Watch {
                path: dir.to_path_buf(),
                source,
            })?;

        Ok(FileWatcher {
            _watcher: watcher,
            rx,
            debounce: Duration::from_millis(debounce_ms),
            pending: None,
        })
    }

    /// Whether this is a write event for a `.map` file.
    fn is_map_write(event: &Event) -> bool {
        let is_write = matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_));
        is_write
            && event
                .paths
                .iter()
                .any(|p| p.extension().map(|e| e == "map").unwrap_or(false))
    }

    /// Non-blocking poll: return the change path confirmed after debounce.
    ///
    /// Multiple writes to the same path within the debounce window trigger
    /// only once.
    pub fn poll(&mut self) -> Option<PathBuf> {
        // First drain the channel and collect the latest events.
        loop {
            match self.rx.try_recv() {
                Ok(WatchEvent::MapChanged(path)) => {
                    let now = Instant::now();
                    match &self.pending {
                        Some((_, at)) if now.duration_since(*at) < self.debounce => {
                            // Within the debounce window; update the path.
                            self.pending = Some((path, *at));
                        }
                        _ => {
                            let ready = self.pending.take().map(|(p, _)| p);
                            self.pending = Some((path, now));
                            if let Some(ready) = ready {
                                return Some(ready);
                            }
                        }
                    }
                }
                Ok(WatchEvent::Error(e)) => {
                    warn!(error = %e, "file watcher error");
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        // Check whether pending has passed the debounce window.
        if let Some((path, at)) = &self.pending {
            if at.elapsed() >= self.debounce {
                let path = path.clone();
                self.pending = None;
                debug!(path = %path.display(), "map change debounced");
                return Some(path);
            }
        }

        None
    }

    /// Block until the next change (sleeps until the debounce window ends).
    pub fn wait(&mut self) -> Option<PathBuf> {
        self.wait_timeout(Duration::from_secs(60 * 60))
    }

    /// Blocking wait with a timeout.
    ///
    /// Returns `None` on timeout. Used by tests and by host loops that reload
    /// only when something changed.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Option<PathBuf> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(path) = self.poll() {
                return Some(path);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            // Sleep for min(10ms, remaining time).
            let slice = (deadline - now).min(Duration::from_millis(10));
            match self.rx.recv_timeout(slice) {
                Ok(WatchEvent::MapChanged(path)) => {
                    self.pending = Some((path, Instant::now()));
                }
                Ok(WatchEvent::Error(e)) => {
                    warn!(error = %e, "file watcher error");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}
