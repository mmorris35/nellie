//! File system event types and handling.

#![allow(clippy::missing_const_for_fn)]

use std::path::PathBuf;

/// File system event types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvent {
    /// File was created or modified.
    Modified(PathBuf),
    /// File was deleted.
    Deleted(PathBuf),
    /// File was renamed from old path to new path.
    Renamed { from: PathBuf, to: PathBuf },
}

impl FileEvent {
    /// Get the primary path associated with this event.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::Modified(p) | Self::Deleted(p) => p,
            Self::Renamed { to, .. } => to,
        }
    }

    /// Check if this event affects a file (vs directory).
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.path().is_file() || !self.path().exists()
    }
}

/// Batch of file events for processing.
#[derive(Debug, Default)]
pub struct EventBatch {
    /// Modified files (need re-indexing).
    pub modified: Vec<PathBuf>,
    /// Deleted files (need removal from index).
    pub deleted: Vec<PathBuf>,
}

impl EventBatch {
    /// Create a new empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an event to the batch.
    pub fn add(&mut self, event: FileEvent) {
        match event {
            FileEvent::Modified(path) => {
                if !self.modified.contains(&path) {
                    self.modified.push(path);
                }
            }
            FileEvent::Deleted(path) => {
                // Remove from modified if present
                self.modified.retain(|p| p != &path);
                if !self.deleted.contains(&path) {
                    self.deleted.push(path);
                }
            }
            FileEvent::Renamed { from, to } => {
                // Treat as delete + create
                self.modified.retain(|p| p != &from);
                if !self.deleted.contains(&from) {
                    self.deleted.push(from);
                }
                if !self.modified.contains(&to) {
                    self.modified.push(to);
                }
            }
        }
    }

    /// Check if batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modified.is_empty() && self.deleted.is_empty()
    }

    /// Get total number of events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.modified.len() + self.deleted.len()
    }

    /// Clear all events.
    pub fn clear(&mut self) {
        self.modified.clear();
        self.deleted.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_path() {
        let modified = FileEvent::Modified(PathBuf::from("/test/file.rs"));
        assert_eq!(modified.path(), &PathBuf::from("/test/file.rs"));

        let deleted = FileEvent::Deleted(PathBuf::from("/test/removed.rs"));
        assert_eq!(deleted.path(), &PathBuf::from("/test/removed.rs"));

        let renamed = FileEvent::Renamed {
            from: PathBuf::from("/old.rs"),
            to: PathBuf::from("/new.rs"),
        };
        assert_eq!(renamed.path(), &PathBuf::from("/new.rs"));
    }

    #[test]
    fn test_event_batch_add_modified() {
        let mut batch = EventBatch::new();
        batch.add(FileEvent::Modified(PathBuf::from("/a.rs")));
        batch.add(FileEvent::Modified(PathBuf::from("/b.rs")));
        batch.add(FileEvent::Modified(PathBuf::from("/a.rs"))); // Duplicate

        assert_eq!(batch.modified.len(), 2);
        assert!(batch.deleted.is_empty());
    }

    #[test]
    fn test_event_batch_add_deleted() {
        let mut batch = EventBatch::new();
        batch.add(FileEvent::Modified(PathBuf::from("/a.rs")));
        batch.add(FileEvent::Deleted(PathBuf::from("/a.rs")));

        assert!(batch.modified.is_empty());
        assert_eq!(batch.deleted.len(), 1);
    }

    #[test]
    fn test_event_batch_renamed() {
        let mut batch = EventBatch::new();
        batch.add(FileEvent::Renamed {
            from: PathBuf::from("/old.rs"),
            to: PathBuf::from("/new.rs"),
        });

        assert_eq!(batch.modified, vec![PathBuf::from("/new.rs")]);
        assert_eq!(batch.deleted, vec![PathBuf::from("/old.rs")]);
    }

    #[test]
    fn test_event_batch_len_and_empty() {
        let mut batch = EventBatch::new();
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);

        batch.add(FileEvent::Modified(PathBuf::from("/a.rs")));
        batch.add(FileEvent::Deleted(PathBuf::from("/b.rs")));

        assert!(!batch.is_empty());
        assert_eq!(batch.len(), 2);
    }
}
