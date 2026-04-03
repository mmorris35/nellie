//! File change event handler.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use super::events::EventBatch;
use super::filter::FileFilter;
use crate::Result;

/// Statistics for file watching.
#[derive(Debug, Default)]
pub struct WatcherStats {
    pub files_detected: AtomicU64,
    pub files_filtered: AtomicU64,
    pub files_indexed: AtomicU64,
    pub files_deleted: AtomicU64,
    pub errors: AtomicU64,
}

impl WatcherStats {
    /// Create new stats tracker.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Get snapshot of current stats.
    #[must_use]
    pub fn snapshot(&self) -> WatcherStatsSnapshot {
        WatcherStatsSnapshot {
            files_detected: self.files_detected.load(Ordering::Relaxed),
            files_filtered: self.files_filtered.load(Ordering::Relaxed),
            files_indexed: self.files_indexed.load(Ordering::Relaxed),
            files_deleted: self.files_deleted.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of watcher stats.
#[derive(Debug, Clone, Copy)]
pub struct WatcherStatsSnapshot {
    pub files_detected: u64,
    pub files_filtered: u64,
    pub files_indexed: u64,
    pub files_deleted: u64,
    pub errors: u64,
}

/// Processed file event for indexing.
#[derive(Debug, Clone)]
pub struct IndexRequest {
    /// File path.
    pub path: PathBuf,
    /// Detected language.
    pub language: Option<String>,
}

/// Event handler configuration.
#[derive(Debug, Clone)]
pub struct HandlerConfig {
    /// Base path for filtering.
    pub base_path: PathBuf,
    /// Custom ignore patterns.
    pub ignore_patterns: Vec<String>,
}

/// Event handler that filters and processes file changes.
pub struct EventHandler {
    filter: FileFilter,
    stats: Arc<WatcherStats>,
    index_tx: mpsc::Sender<IndexRequest>,
    delete_tx: mpsc::Sender<PathBuf>,
}

impl EventHandler {
    /// Create a new event handler.
    ///
    /// # Errors
    ///
    /// Returns an error if the filter cannot be created.
    pub fn new(
        config: &HandlerConfig,
        stats: Arc<WatcherStats>,
        index_tx: mpsc::Sender<IndexRequest>,
        delete_tx: mpsc::Sender<PathBuf>,
    ) -> Result<Self> {
        let patterns: Vec<&str> = config.ignore_patterns.iter().map(String::as_str).collect();
        let filter = if patterns.is_empty() {
            FileFilter::new(&config.base_path)
        } else {
            FileFilter::with_patterns(&config.base_path, &patterns)?
        };

        Ok(Self {
            filter,
            stats,
            index_tx,
            delete_tx,
        })
    }

    /// Process a batch of file events.
    pub async fn process_batch(&self, batch: EventBatch) {
        let total = batch.len();
        self.stats
            .files_detected
            .fetch_add(total as u64, Ordering::Relaxed);

        // Process modified files
        for path in batch.modified {
            if self.filter.should_index(&path) {
                let language = FileFilter::detect_language(&path).map(String::from);
                let request = IndexRequest {
                    path: path.clone(),
                    language,
                };

                if self.index_tx.send(request).await.is_ok() {
                    self.stats.files_indexed.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                self.stats.files_filtered.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Process deleted files
        for path in batch.deleted {
            if self.delete_tx.send(path).await.is_ok() {
                self.stats.files_deleted.fetch_add(1, Ordering::Relaxed);
            } else {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }

        let snapshot = self.stats.snapshot();
        tracing::debug!(
            detected = snapshot.files_detected,
            indexed = snapshot.files_indexed,
            filtered = snapshot.files_filtered,
            deleted = snapshot.files_deleted,
            "Processed event batch"
        );
    }

    /// Get current stats.
    #[must_use]
    pub fn stats(&self) -> Arc<WatcherStats> {
        Arc::clone(&self.stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_handler_filters_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();

        let stats = WatcherStats::new();
        let (index_tx, mut index_rx) = mpsc::channel(10);
        let (delete_tx, _delete_rx) = mpsc::channel(10);

        let config = HandlerConfig {
            base_path: tmp.path().to_path_buf(),
            ignore_patterns: vec![],
        };

        let handler = EventHandler::new(&config, stats.clone(), index_tx, delete_tx).unwrap();

        let mut batch = EventBatch::new();
        batch.modified.push(tmp.path().join("main.rs"));
        batch.modified.push(tmp.path().join("image.png")); // Should be filtered

        handler.process_batch(batch).await;

        // Should only receive the .rs file
        let request = index_rx.recv().await.unwrap();
        assert!(request.path.ends_with("main.rs"));
        assert_eq!(request.language, Some("rust".to_string()));

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.files_detected, 2);
        assert_eq!(snapshot.files_indexed, 1);
        assert_eq!(snapshot.files_filtered, 1);
    }

    #[tokio::test]
    async fn test_handler_processes_deletes() {
        let tmp = TempDir::new().unwrap();

        let stats = WatcherStats::new();
        let (index_tx, _index_rx) = mpsc::channel(10);
        let (delete_tx, mut delete_rx) = mpsc::channel(10);

        let config = HandlerConfig {
            base_path: tmp.path().to_path_buf(),
            ignore_patterns: vec![],
        };

        let handler = EventHandler::new(&config, stats.clone(), index_tx, delete_tx).unwrap();

        let mut batch = EventBatch::new();
        batch.deleted.push(tmp.path().join("deleted.rs"));

        handler.process_batch(batch).await;

        let path = delete_rx.recv().await.unwrap();
        assert!(path.ends_with("deleted.rs"));

        assert_eq!(stats.snapshot().files_deleted, 1);
    }
}
