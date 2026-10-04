//! 文件监听：debounce 后触发重载事件。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{debug, warn};

/// 监听事件。
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// `.map` 文件被修改，需要重载。
    MapChanged(PathBuf),
    /// 监听出错。
    Error(String),
}

/// `.map` 文件监听器（带 debounce）。
pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<WatchEvent>,
    debounce: Duration,
    /// 上次事件时间（用于 debounce）。
    pending: Option<(PathBuf, Instant)>,
}

/// Watcher 错误。
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
    /// 创建监听器，监听 `dir` 下的 `.map` 文件。
    ///
    /// `debounce_ms` 默认 100ms。
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

    /// 是否是 `.map` 的写入事件。
    fn is_map_write(event: &Event) -> bool {
        let is_write = matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_));
        is_write
            && event
                .paths
                .iter()
                .any(|p| p.extension().map(|e| e == "map").unwrap_or(false))
    }

    /// 非阻塞轮询：返回 debounce 后确认的变更路径。
    ///
    /// 同一路径在 debounce 窗口内的多次写入只触发一次。
    pub fn poll(&mut self) -> Option<PathBuf> {
        // 先清空 channel，收集最新事件
        loop {
            match self.rx.try_recv() {
                Ok(WatchEvent::MapChanged(path)) => {
                    let now = Instant::now();
                    match &self.pending {
                        Some((_, at)) if now.duration_since(*at) < self.debounce => {
                            // 在 debounce 窗口内，更新路径
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

        // 检查 pending 是否已过 debounce 窗口
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

    /// 阻塞等待下一个变更（会睡到 debounce 窗口结束）。
    pub fn wait(&mut self) -> Option<PathBuf> {
        self.wait_timeout(Duration::from_secs(60 * 60))
    }

    /// 带超时的阻塞等待。
    ///
    /// 超时返回 `None`。用于测试与"只在有变化时才重载"的宿主循环。
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
            // 睡到 min(10ms, 剩余时间)。
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
