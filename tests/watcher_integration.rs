//! Integration tests for file watcher and indexer.

use nellie::storage::{count_chunks, init_storage, Database};
use std::fs;
use tempfile::TempDir;

/// Test that initial scan indexes files.
#[tokio::test]
async fn test_initial_scan_indexes_files() {
    // Create temp directory with test files
    let tmp = TempDir::new().unwrap();
    let code_dir = tmp.path().join("code");
    fs::create_dir_all(&code_dir).unwrap();

    // Write some test files
    fs::write(
        code_dir.join("main.rs"),
        r#"fn main() {
    println!("Hello, world!");
}
"#,
    )
    .unwrap();

    fs::write(
        code_dir.join("lib.rs"),
        r#"pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 2), 4);
    }
}
"#,
    )
    .unwrap();

    // Set up database
    let db_path = tmp.path().join("test.db");
    let db = Database::open(&db_path).unwrap();
    init_storage(&db).unwrap();

    // Create indexer without embeddings
    let indexer = nellie::watcher::Indexer::new(db.clone(), None, false);

    // Index files manually
    for entry in walkdir::WalkDir::new(&code_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |e| e == "rs") {
            let request = nellie::watcher::IndexRequest {
                path: path.to_path_buf(),
                language: Some("rust".to_string()),
            };
            indexer.index_file(&request).await.unwrap();
        }
    }

    // Verify chunks were indexed
    let chunk_count = db.with_conn(count_chunks).unwrap();
    assert!(chunk_count > 0, "Expected chunks to be indexed, got 0");

    // Should have indexed at least 2 chunks (one per file minimum)
    assert!(
        chunk_count >= 2,
        "Expected at least 2 chunks, got {}",
        chunk_count
    );
}

/// Test that file filter excludes non-code files.
#[test]
fn test_file_filter_excludes_binaries() {
    let tmp = TempDir::new().unwrap();

    // Create various files
    fs::write(tmp.path().join("code.rs"), "fn main() {}").unwrap();
    fs::write(tmp.path().join("image.png"), &[0u8; 100]).unwrap();
    fs::write(tmp.path().join("data.json"), "{}").unwrap();

    let filter = nellie::watcher::FileFilter::new(tmp.path());

    assert!(filter.should_index(&tmp.path().join("code.rs")));
    assert!(!filter.should_index(&tmp.path().join("image.png")));
    // JSON is a supported code file
    assert!(filter.should_index(&tmp.path().join("data.json")));
}

/// Test that file filter respects gitignore.
#[test]
fn test_file_filter_respects_gitignore() {
    let tmp = TempDir::new().unwrap();

    // Create .gitignore file
    fs::write(tmp.path().join(".gitignore"), "*.log\ntest_output/\n").unwrap();

    // Create test files
    fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();
    fs::write(tmp.path().join("debug.log"), "log content").unwrap();

    let filter = nellie::watcher::FileFilter::new(tmp.path());

    assert!(filter.should_index(&tmp.path().join("main.rs")));
    assert!(!filter.should_index(&tmp.path().join("debug.log")));
}

/// Test that watcher stats track correctly.
#[test]
fn test_watcher_stats_tracking() {
    let stats = nellie::watcher::WatcherStats::new();

    // Initially all zeros
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.files_detected, 0);
    assert_eq!(snapshot.files_indexed, 0);
    assert_eq!(snapshot.files_filtered, 0);
    assert_eq!(snapshot.files_deleted, 0);
    assert_eq!(snapshot.errors, 0);

    // Update stats
    stats
        .files_detected
        .fetch_add(10, std::sync::atomic::Ordering::Relaxed);
    stats
        .files_indexed
        .fetch_add(8, std::sync::atomic::Ordering::Relaxed);
    stats
        .files_filtered
        .fetch_add(2, std::sync::atomic::Ordering::Relaxed);

    let snapshot = stats.snapshot();
    assert_eq!(snapshot.files_detected, 10);
    assert_eq!(snapshot.files_indexed, 8);
    assert_eq!(snapshot.files_filtered, 2);
}

/// Test event batch filtering by path.
#[test]
fn test_event_batch_filtering() {
    use std::path::PathBuf;

    let mut batch = nellie::watcher::EventBatch::new();
    batch
        .modified
        .push(PathBuf::from("/home/project/src/main.rs"));
    batch
        .modified
        .push(PathBuf::from("/home/project/src/lib.rs"));
    batch
        .deleted
        .push(PathBuf::from("/home/project/tests/test.rs"));

    assert_eq!(batch.len(), 3);
    assert!(!batch.is_empty());

    // Filter for src directory
    let base_path = PathBuf::from("/home/project/src");
    let filtered_modified: Vec<_> = batch
        .modified
        .iter()
        .filter(|p| p.starts_with(&base_path))
        .cloned()
        .collect();

    assert_eq!(filtered_modified.len(), 2);
}

/// Test structural indexer integration: indexes symbols and skips unchanged files.
#[tokio::test]
async fn test_structural_indexer_symbols() {
    // Create temp directory with test Python file
    let tmp = TempDir::new().unwrap();
    let code_dir = tmp.path().join("code");
    fs::create_dir_all(&code_dir).unwrap();

    // Write a test Python file with a class and function
    fs::write(
        code_dir.join("example.py"),
        r#"class Calculator:
    def add(self, a, b):
        return a + b

def multiply(x, y):
    return x * y
"#,
    )
    .unwrap();

    // Set up database
    let db_path = tmp.path().join("test_structural.db");
    let db = Database::open(&db_path).unwrap();
    init_storage(&db).unwrap();

    // Create indexer with enable_structural=true
    let indexer = nellie::watcher::Indexer::new(db.clone(), None, true);

    // Index the file
    let file_path = code_dir.join("example.py");
    let request = nellie::watcher::IndexRequest {
        path: file_path.clone(),
        language: Some("python".to_string()),
    };
    indexer.index_file(&request).await.unwrap();

    // Query symbols and verify they were stored
    let symbols = db
        .with_conn(|conn| {
            nellie::structural::storage::query_symbols_by_file(conn, file_path.to_str().unwrap())
        })
        .unwrap();
    assert!(
        symbols.len() > 0,
        "Should have indexed at least one symbol (class or function)"
    );

    // Verify we have a Calculator class and multiply function
    let has_class = symbols.iter().any(|s| s.symbol_name == "Calculator");
    let has_function = symbols.iter().any(|s| s.symbol_name == "multiply");
    assert!(has_class, "Should have indexed Calculator class");
    assert!(has_function, "Should have indexed multiply function");

    let original_count = symbols.len();

    // Re-index the same file (unchanged content) and verify skip logic
    let request2 = nellie::watcher::IndexRequest {
        path: file_path.clone(),
        language: Some("python".to_string()),
    };
    indexer.index_file(&request2).await.unwrap();

    // Query symbols again and verify count is the same
    let symbols2 = db
        .with_conn(|conn| {
            nellie::structural::storage::query_symbols_by_file(conn, file_path.to_str().unwrap())
        })
        .unwrap();
    assert_eq!(
        symbols2.len(),
        original_count,
        "Re-indexing unchanged file should not add duplicate symbols"
    );
}
