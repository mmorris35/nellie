//! File system watcher using notify-rs.

#![allow(clippy::used_underscore_binding)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind, Debouncer};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::events::{EventBatch, FileEvent};
use crate::error::WatcherError;
use crate::Result;

/// Debounce duration for file events.
const DEBOUNCE_DURATION: Duration = Duration::from_millis(500);

/// Directories to skip when setting up watches.
/// (Used by tests; handler's FileFilter handles event filtering)
#[allow(dead_code)]
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "build",
    "dist",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    "vendor",
    ".next",
    ".nuxt",
    "coverage",
    ".cache",
    ".parcel-cache",
    "bower_components",
];

/// File watcher configuration.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Directories to watch.
    pub watch_dirs: Vec<PathBuf>,
    /// Debounce duration.
    pub debounce: Duration,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            watch_dirs: Vec::new(),
            debounce: DEBOUNCE_DURATION,
        }
    }
}

/// File system watcher.
pub struct FileWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
    event_rx: mpsc::Receiver<EventBatch>,
    watched_dirs: Arc<Mutex<Vec<PathBuf>>>,
}

impl FileWatcher {
    /// Create a new file watcher.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher cannot be created.
    pub fn new(config: &WatcherConfig) -> Result<Self> {
        let (batch_tx, event_rx) = mpsc::channel(100);
        let watched_dirs = Arc::new(Mutex::new(Vec::new()));
        let watched_dirs_clone = Arc::clone(&watched_dirs);

        let debouncer = new_debouncer(
            config.debounce,
            move |result: std::result::Result<
                Vec<notify_debouncer_mini::DebouncedEvent>,
                notify::Error,
            >| {
                match result {
                    Ok(events) => {
                        let mut batch = EventBatch::new();
                        for event in events {
                            if matches!(event.kind, DebouncedEventKind::Any) {
                                batch.add(FileEvent::Modified(event.path));
                            }
                        }

                        if !batch.is_empty() {
                            // Filter to watched directories
                            let dirs = watched_dirs_clone.lock();
                            batch.modified.retain(|p| is_under_watched(&dirs, p));
                            batch.deleted.retain(|p| is_under_watched(&dirs, p));

                            if !batch.is_empty() {
                                let _ = batch_tx.blocking_send(batch);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Watch error: {:?}", e);
                    }
                }
            },
        )
        .map_err(|e| WatcherError::WatchFailed {
            path: "init".to_string(),
            reason: e.to_string(),
        })?;

        let mut watcher = Self {
            _debouncer: debouncer,
            event_rx,
            watched_dirs,
        };

        // Add initial watch directories
        for dir in &config.watch_dirs {
            watcher.watch(dir)?;
        }

        Ok(watcher)
    }

    /// Add a directory to watch.
    ///
    /// Uses FSEvents recursive mode for efficiency. Events from ignored
    /// directories (node_modules, .git, etc.) are filtered in the event handler.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be watched.
    pub fn watch(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            return Err(WatcherError::WatchFailed {
                path: path.display().to_string(),
                reason: "directory does not exist".to_string(),
            }
            .into());
        }

        // Use Recursive mode - FSEvents handles this efficiently
        // Events from ignored directories will be filtered in the event handler
        self._debouncer
            .watcher()
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|e| WatcherError::WatchFailed {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;

        self.watched_dirs.lock().push(path.clone());
        tracing::info!(
            path = %path.display(),
            "Watching directory tree (recursive mode)"
        );

        Ok(())
    }

    /// Stop watching a directory.
    ///
    /// # Errors
    ///
    /// Returns an error if unwatching fails.
    pub fn unwatch(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        self._debouncer
            .watcher()
            .unwatch(path)
            .map_err(|e| WatcherError::WatchFailed {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;

        self.watched_dirs.lock().retain(|p| p != path);

        tracing::info!(path = %path.display(), "Stopped watching directory");
        Ok(())
    }

    /// Receive the next batch of events.
    ///
    /// Returns `None` if the watcher has been dropped.
    pub async fn recv(&mut self) -> Option<EventBatch> {
        self.event_rx.recv().await
    }

    /// Get list of watched directories.
    #[must_use]
    pub fn watched_dirs(&self) -> Vec<PathBuf> {
        self.watched_dirs.lock().clone()
    }
}

/// Check if a path is under any watched directory.
fn is_under_watched(watched: &[PathBuf], path: &Path) -> bool {
    watched.iter().any(|dir| path.starts_with(dir))
}

/// Check if a directory should be skipped from watching.
/// (Used by tests; handler's FileFilter handles event filtering)
#[allow(dead_code)]
fn should_skip_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| SKIP_DIRS.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_watcher_config_default() {
        let config = WatcherConfig::default();
        assert!(config.watch_dirs.is_empty());
        assert_eq!(config.debounce, DEBOUNCE_DURATION);
    }

    #[test]
    fn test_is_under_watched() {
        let watched = vec![
            PathBuf::from("/home/user/project"),
            PathBuf::from("/var/data"),
        ];

        assert!(is_under_watched(
            &watched,
            Path::new("/home/user/project/src/main.rs")
        ));
        assert!(is_under_watched(&watched, Path::new("/var/data/file.txt")));
        assert!(!is_under_watched(&watched, Path::new("/tmp/other.txt")));
    }

    #[test]
    fn test_watcher_nonexistent_dir() {
        let config = WatcherConfig::default();
        let mut watcher = FileWatcher::new(&config).unwrap();

        let result = watcher.watch("/nonexistent/directory");
        assert!(result.is_err());
    }

    #[test]
    fn test_watcher_watch_and_unwatch() {
        let tmp = TempDir::new().unwrap();
        let config = WatcherConfig::default();
        let mut watcher = FileWatcher::new(&config).unwrap();

        watcher.watch(tmp.path()).unwrap();
        assert_eq!(watcher.watched_dirs().len(), 1);

        watcher.unwatch(tmp.path()).unwrap();
        assert!(watcher.watched_dirs().is_empty());
    }

    #[test]
    fn test_should_skip_dir() {
        assert!(should_skip_dir(Path::new("/project/node_modules")));
        assert!(should_skip_dir(Path::new("/project/.git")));
        assert!(should_skip_dir(Path::new("/project/target")));
        assert!(!should_skip_dir(Path::new("/project/src")));
        assert!(!should_skip_dir(Path::new("/project/lib")));
    }
}
