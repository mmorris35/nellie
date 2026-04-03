//! File system watching and indexing.
//!
//! This module provides:
//! - Directory watching using notify-rs
//! - Gitignore-aware file filtering
//! - Incremental indexing of changed files
//! - Directory scanning for initial indexing

mod chunker;
mod events;
mod filter;
mod handler;
mod indexer;
mod scanner;
#[allow(clippy::module_inception)]
mod watcher;

pub use chunker::{Chunker, ChunkerConfig, CodeChunk};
pub use events::EventBatch;
pub use events::FileEvent;
pub use filter::FileFilter;
pub use handler::{EventHandler, HandlerConfig, IndexRequest, WatcherStats, WatcherStatsSnapshot};
pub use indexer::Indexer;
pub use scanner::{scan_directory, scan_directory_async, ScanStats, ScanStatsSnapshot};
pub use watcher::{FileWatcher, WatcherConfig};

/// Initialize watcher module.
pub fn init() {
    tracing::debug!("Watcher module initialized");
}
