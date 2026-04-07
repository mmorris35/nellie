//! Directory scanner for initial indexing.
//!
//! Walks directories respecting .gitignore and sends files for indexing.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use ignore::WalkBuilder;
use tokio::sync::mpsc;

use super::filter::FileFilter;
use super::handler::IndexRequest;
use crate::Result;

/// Scan statistics.
#[derive(Debug, Default)]
pub struct ScanStats {
    pub files_found: AtomicU64,
    pub files_queued: AtomicU64,
    pub files_skipped: AtomicU64,
    pub errors: AtomicU64,
}

impl ScanStats {
    /// Create new stats.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a snapshot of current stats.
    #[must_use]
    pub fn snapshot(&self) -> ScanStatsSnapshot {
        ScanStatsSnapshot {
            files_found: self.files_found.load(Ordering::Relaxed),
            files_queued: self.files_queued.load(Ordering::Relaxed),
            files_skipped: self.files_skipped.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of scan stats.
#[derive(Debug, Clone, Copy)]
pub struct ScanStatsSnapshot {
    pub files_found: u64,
    pub files_queued: u64,
    pub files_skipped: u64,
    pub errors: u64,
}

/// Scan a directory and queue files for indexing.
///
/// Uses the `ignore` crate to respect .gitignore patterns.
/// Filters to code files only using `FileFilter`.
///
/// Returns statistics about the scan.
pub fn scan_directory(
    path: &Path,
    index_tx: &mpsc::Sender<IndexRequest>,
) -> Result<ScanStatsSnapshot> {
    let stats = ScanStats::new();

    tracing::info!(path = %path.display(), "Starting directory scan");

    let walker = WalkBuilder::new(path)
        .hidden(true) // Respect hidden files/dirs
        .git_ignore(true) // Respect .gitignore
        .git_global(true) // Respect global gitignore
        .git_exclude(true) // Respect .git/info/exclude
        .ignore(true) // Respect .ignore files
        .parents(true) // Check parent directories for ignore files
        .filter_entry(|entry| {
            // Skip excluded directories at walk-time to avoid traversing
            // into node_modules/, target/, etc. (can contain 97K+ files)
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !super::filter::is_excluded_dir(name);
                }
            }
            true
        })
        .build();

    for entry in walker {
        match entry {
            Ok(entry) => {
                let entry_path = entry.path();

                // Skip directories
                if entry_path.is_dir() {
                    continue;
                }

                stats.files_found.fetch_add(1, Ordering::Relaxed);

                // Check if it's a code file we should index
                if !FileFilter::is_code_file(entry_path) {
                    stats.files_skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                // Check additional ignore patterns
                if is_default_ignored(entry_path, Some(path)) {
                    stats.files_skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                // Skip files larger than 1MB
                if let Ok(metadata) = entry_path.metadata() {
                    if metadata.len() > 1_048_576 {
                        stats.files_skipped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                }

                // Detect language and queue for indexing
                let language = FileFilter::detect_language(entry_path).map(String::from);
                let request = IndexRequest {
                    path: entry_path.to_path_buf(),
                    language,
                };

                if index_tx.blocking_send(request).is_err() {
                    tracing::warn!("Index channel closed during scan");
                    break;
                }

                stats.files_queued.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                tracing::warn!(error = %e, "Error walking directory");
                stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let snapshot = stats.snapshot();
    tracing::info!(
        path = %path.display(),
        found = snapshot.files_found,
        queued = snapshot.files_queued,
        skipped = snapshot.files_skipped,
        errors = snapshot.errors,
        "Directory scan complete"
    );

    Ok(snapshot)
}

/// Async version of directory scan.
pub async fn scan_directory_async(
    path: &Path,
    index_tx: &mpsc::Sender<IndexRequest>,
) -> Result<ScanStatsSnapshot> {
    let path = path.to_path_buf();
    let tx = index_tx.clone();

    tokio::task::spawn_blocking(move || scan_directory(&path, &tx))
        .await
        .map_err(|e| crate::Error::internal(format!("Scan task failed: {e}")))?
}

/// Check if a path should be ignored (beyond .gitignore).
///
/// If `base` is provided, only path components relative to the base are checked.
/// This avoids false positives from system temp directories like `/tmp/.tmpXXXXXX`.
fn is_default_ignored(path: &Path, base: Option<&Path>) -> bool {
    let check_path = base.map_or(path, |b| path.strip_prefix(b).unwrap_or(path));

    // Skip dotdirs (except .github)
    for component in check_path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') && name_str.len() > 1 && name_str != ".github" {
                return true;
            }
        }
    }

    // Check against shared exclusion list (node_modules, target, etc.)
    for component in check_path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if super::filter::is_excluded_dir(&name_str) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_is_default_ignored() {
        assert!(is_default_ignored(Path::new("/project/.git/config"), None));
        assert!(is_default_ignored(
            Path::new("/project/node_modules/pkg/index.js"),
            None
        ));
        assert!(is_default_ignored(
            Path::new("/project/target/debug/main"),
            None
        ));
        assert!(!is_default_ignored(Path::new("/project/src/main.rs"), None));
        assert!(!is_default_ignored(
            Path::new("/project/.github/workflows/ci.yml"),
            None
        ));
    }

    #[tokio::test]
    async fn test_scan_directory() {
        let tmp = TempDir::new().unwrap();

        // Create test files
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src.join("lib.rs"), "pub fn hello() {}").unwrap();

        // Create ignored files
        fs::write(tmp.path().join("README.md"), "# Readme").unwrap();
        let node_modules = tmp.path().join("node_modules");
        fs::create_dir(&node_modules).unwrap();
        fs::write(node_modules.join("pkg.js"), "module.exports = {};").unwrap();

        let (tx, mut rx) = mpsc::channel(100);

        // Use scan_directory_async to avoid blocking_send inside tokio runtime
        let stats = scan_directory_async(tmp.path(), &tx).await.unwrap();

        // Should have found and queued the .rs files
        assert!(stats.files_queued >= 2);

        // Check that we received the requests
        drop(tx);
        let mut paths = vec![];
        while let Some(req) = rx.recv().await {
            paths.push(req.path);
        }

        assert!(paths.iter().any(|p| p.ends_with("main.rs")));
        assert!(paths.iter().any(|p| p.ends_with("lib.rs")));
        // node_modules should be ignored
        assert!(!paths
            .iter()
            .any(|p| p.to_string_lossy().contains("node_modules")));
    }
}
