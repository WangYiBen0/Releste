//! File watching: trigger a reload event after debounce.
//!
//! The watcher is generic over *what* counts as a relevant write: the map watcher
//! filters `.map` files, the settings watcher filters the exact config files it was
//! given (they may not exist yet, so it watches their parent directories and matches
//! by path).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, warn};

/// Watch event.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A watched file was modified and needs a reload.
    Changed(PathBuf),
    /// The watcher errored.
    Error(String),
}

/// File watcher (with debounce).
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
    /// Create a watcher that watches `.map` files anywhere under `dir`.
    ///
    /// `debounce_ms` defaults to 100ms.
    pub fn new(dir: &Path, debounce_ms: u64) -> Result<Self, WatcherError> {
        Self::new_filtered(dir, debounce_ms, RecursiveMode::Recursive, |path: &Path| {
            path.extension().map(|e| e == "map").unwrap_or(false)
        })
    }

    /// Create a watcher for an explicit set of files.
    ///
    /// Every distinct parent directory is watched non-recursively, and an event only
    /// counts when it names one of `files`. Watching the parent rather than the file
    /// is deliberate: a settings file the user has not created yet must still be
    /// picked up on its first save.
    pub fn new_files(files: &[PathBuf], debounce_ms: u64) -> Result<Self, WatcherError> {
        let wanted: HashSet<PathBuf> = files.iter().cloned().collect();
        let mut parents: Vec<PathBuf> = Vec::new();
        for file in &wanted {
            let parent = match file.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => PathBuf::from("."),
            };
            if !parents.contains(&parent) {
                parents.push(parent);
            }
        }

        let (tx, rx) = mpsc::channel();
        let event_tx = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(event) => {
                    if !Self::is_write(&event) {
                        return;
                    }
                    for path in &event.paths {
                        if wanted.contains(path) {
                            let _ = event_tx.send(WatchEvent::Changed(path.clone()));
                        }
                    }
                }
                Err(e) => {
                    let _ = event_tx.send(WatchEvent::Error(e.to_string()));
                }
            })?;

        if parents.is_empty() {
            return Err(WatcherError::Watch {
                path: PathBuf::new(),
                source: notify::Error::generic("no parent directory to watch"),
            });
        }
        for parent in &parents {
            watcher
                .watch(parent, RecursiveMode::NonRecursive)
                .map_err(|source| WatcherError::Watch {
                    path: parent.clone(),
                    source,
                })?;
        }

        Ok(FileWatcher {
            _watcher: watcher,
            rx,
            debounce: Duration::from_millis(debounce_ms),
            pending: None,
        })
    }

    /// Create a watcher with a caller-supplied relevance filter.
    ///
    /// `filter` decides whether a changed path matters; paths it rejects never reach
    /// [`FileWatcher::poll`]. The filter runs on the notify thread, so it must be
    /// cheap.
    pub fn new_filtered<F>(
        dir: &Path,
        debounce_ms: u64,
        mode: RecursiveMode,
        filter: F,
    ) -> Result<Self, WatcherError>
    where
        F: Fn(&Path) -> bool + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();

        let event_tx = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(event) => {
                    if !Self::is_write(&event) {
                        return;
                    }
                    for path in &event.paths {
                        if filter(path) {
                            let _ = event_tx.send(WatchEvent::Changed(path.clone()));
                        }
                    }
                }
                Err(e) => {
                    let _ = event_tx.send(WatchEvent::Error(e.to_string()));
                }
            })?;

        watcher
            .watch(dir, mode)
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

    /// Whether this event is a create/modify (as opposed to a metadata-only touch).
    fn is_write(event: &Event) -> bool {
        matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
    }

    /// Non-blocking poll: return the change path confirmed after debounce.
    ///
    /// Multiple writes to the same path within the debounce window trigger
    /// only once.
    pub fn poll(&mut self) -> Option<PathBuf> {
        // First drain the channel and collect the latest events.
        loop {
            match self.rx.try_recv() {
                Ok(WatchEvent::Changed(path)) => {
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
                debug!(path = %path.display(), "file change debounced");
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
                Ok(WatchEvent::Changed(path)) => {
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
