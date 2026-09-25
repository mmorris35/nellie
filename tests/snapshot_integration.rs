//! Integration tests for portable graph snapshots (Phase 2).
//!
//! Exercises the real public `nellie::snapshot` API against on-disk SQLite
//! databases initialized with the full schema + sqlite-vec, using the real
//! `Indexer` (without ONNX embeddings) to populate chunks/symbols/file_state.

use nellie::snapshot::{export_snapshot, import_snapshot};
use nellie::storage::{count_chunks, init_storage, Database};
use nellie::watcher::{IndexRequest, Indexer};
use std::fs;
use tempfile::TempDir;

/// Index every `.rs` file under `dir` into `db` (no embeddings, structural on).
async fn index_dir(db: &Database, dir: &std::path::Path) {
    let indexer = Indexer::new(db.clone(), None, true);
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            let request = IndexRequest {
                path: path.to_path_buf(),
                language: Some("rust".to_string()),
            };
            indexer.index_file(&request).await.unwrap();
        }
    }
}

#[tokio::test]
async fn test_snapshot_round_trip_restores_index() {
    let tmp = TempDir::new().unwrap();

    // A project with two source files.
    let project = tmp.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();

    // Source DB: index the project.
    let src_db = Database::open(tmp.path().join("source.db")).unwrap();
    init_storage(&src_db).unwrap();
    index_dir(&src_db, &project).await;

    let original_chunks = src_db.with_conn(count_chunks).unwrap();
    assert!(original_chunks > 0, "expected chunks after indexing");

    // Export.
    let snap_path = tmp.path().join(".nellie/graph.db.zst");
    let report = export_snapshot(&src_db, &project, &snap_path).unwrap();
    assert!(snap_path.exists());
    assert!(
        report.files >= 2,
        "expected >=2 files, got {}",
        report.files
    );
    assert_eq!(
        u64::try_from(report.chunks).unwrap(),
        original_chunks as u64
    );
    assert!(
        report.compressed_bytes < report.raw_bytes,
        "snapshot should compress ({} vs {})",
        report.compressed_bytes,
        report.raw_bytes
    );

    // Fresh destination DB (simulates a teammate / clean clone).
    let dst_db = Database::open(tmp.path().join("dest.db")).unwrap();
    init_storage(&dst_db).unwrap();
    assert_eq!(dst_db.with_conn(count_chunks).unwrap(), 0);

    // Import.
    let import = import_snapshot(&dst_db, &snap_path).unwrap();
    assert!(import.files_merged >= 2);
    assert_eq!(import.files_skipped, 0);

    // Chunks restored.
    assert_eq!(dst_db.with_conn(count_chunks).unwrap(), original_chunks);

    // Symbols restored (structural was enabled).
    let sym_count: i64 = dst_db
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
                .map_err(|e| nellie::error::StorageError::Database(e.to_string()).into())
        })
        .unwrap();
    assert!(sym_count > 0, "expected symbols to be restored");
}

#[tokio::test]
async fn test_snapshot_incremental_skip_unchanged() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("a.rs"), "pub fn a() {}\n").unwrap();
    fs::write(project.join("b.rs"), "pub fn b() {}\n").unwrap();

    let src_db = Database::open(tmp.path().join("source.db")).unwrap();
    init_storage(&src_db).unwrap();
    index_dir(&src_db, &project).await;

    let snap_path = tmp.path().join("graph.db.zst");
    export_snapshot(&src_db, &project, &snap_path).unwrap();

    // Destination already has a.rs indexed with the SAME content (same hash),
    // but not b.rs. Import should merge b.rs and skip a.rs.
    let dst_db = Database::open(tmp.path().join("dest.db")).unwrap();
    init_storage(&dst_db).unwrap();
    let indexer = Indexer::new(dst_db.clone(), None, true);
    indexer
        .index_file(&IndexRequest {
            path: project.join("a.rs"),
            language: Some("rust".to_string()),
        })
        .await
        .unwrap();

    let import = import_snapshot(&dst_db, &snap_path).unwrap();
    assert_eq!(
        import.files_skipped, 1,
        "a.rs should be skipped (identical)"
    );
    assert_eq!(import.files_merged, 1, "b.rs should be merged");
}

#[tokio::test]
async fn test_snapshot_import_missing_meta_rejected() {
    let tmp = TempDir::new().unwrap();
    // A zstd-compressed but non-snapshot sqlite file (no snapshot_meta table).
    let raw_db = tmp.path().join("plain.db");
    {
        let db = Database::open(&raw_db).unwrap();
        init_storage(&db).unwrap();
    }
    let bytes = fs::read(&raw_db).unwrap();
    let compressed = zstd::encode_all(bytes.as_slice(), 3).unwrap();
    let bogus = tmp.path().join("bogus.db.zst");
    fs::write(&bogus, compressed).unwrap();

    let dst = Database::open(tmp.path().join("dest.db")).unwrap();
    init_storage(&dst).unwrap();
    let err = import_snapshot(&dst, &bogus).unwrap_err();
    assert!(
        err.to_string().contains("snapshot_meta"),
        "expected missing-meta error, got: {err}"
    );
}
