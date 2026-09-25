//! Portable graph snapshots.
//!
//! Exports a project-scoped, index-only subset of `nellie.db` (chunks,
//! embeddings, symbols, structural edges, graph nodes/edges, file state)
//! as a zstd-compressed SQLite database that teams can commit to their
//! repository (default: `.nellie/graph.db.zst`).
//!
//! Importing merges the snapshot into the local database and relies on the
//! existing `file_state` + `diff_index` machinery so only files changed
//! since the snapshot need reindexing.
//!
//! Machine-local tables (`lessons`, `checkpoints`, `agent_status`,
//! `watch_dirs`) are deliberately excluded — a snapshot is codebase index
//! data, not agent memory.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::error::StorageError;
use crate::storage::{Database, EMBEDDING_DIM, SCHEMA_VERSION};
use crate::{Error, Result};

/// Default snapshot location relative to the project root.
pub const SNAPSHOT_DEFAULT_REL_PATH: &str = ".nellie/graph.db.zst";

/// zstd compression level for snapshots (default level; good ratio/speed).
const ZSTD_LEVEL: i32 = 3;

/// Report from a snapshot export.
#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    /// Project root the snapshot was scoped to.
    pub project_root: String,
    /// Output path of the compressed snapshot.
    pub out_path: String,
    /// Files (file_state rows) included.
    pub files: u64,
    /// Code chunks included.
    pub chunks: u64,
    /// Chunk embeddings included.
    pub embeddings: u64,
    /// Structural symbols included.
    pub symbols: u64,
    /// Structural edges included.
    pub structural_edges: u64,
    /// Semantic graph nodes included.
    pub graph_nodes: u64,
    /// Semantic graph edges included.
    pub graph_edges: u64,
    /// Uncompressed snapshot DB size in bytes.
    pub raw_bytes: u64,
    /// Compressed artifact size in bytes.
    pub compressed_bytes: u64,
}

/// Report from a snapshot import.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    /// Snapshot file that was imported.
    pub in_path: String,
    /// Files merged (new or hash differed from local).
    pub files_merged: u64,
    /// Files skipped (local index already has the identical hash).
    pub files_skipped: u64,
    /// Chunks inserted.
    pub chunks_inserted: u64,
    /// Embeddings inserted.
    pub embeddings_inserted: u64,
    /// Symbols inserted.
    pub symbols_inserted: u64,
    /// Structural edges inserted.
    pub structural_edges_inserted: u64,
    /// Graph nodes merged (existing local nodes are kept).
    pub graph_nodes_merged: u64,
    /// Graph edges merged (existing local edges are kept).
    pub graph_edges_merged: u64,
}

/// Resolve the default snapshot path for a project root.
#[must_use]
pub fn default_snapshot_path(project_root: &Path) -> PathBuf {
    project_root.join(SNAPSHOT_DEFAULT_REL_PATH)
}

/// SQL `LIKE` pattern matching all files under a project root.
fn project_prefix_pattern(project_root: &Path) -> String {
    let root = project_root.to_string_lossy();
    let root = root.trim_end_matches('/');
    format!("{root}/%")
}

/// Map a rusqlite error into a storage error.
fn db_err(context: &str, e: &rusqlite::Error) -> Error {
    StorageError::Database(format!("{context}: {e}")).into()
}

/// Export a project-scoped snapshot of the index to `out_path`.
///
/// Builds a filtered copy of the index tables in a temporary SQLite
/// database, zstd-compresses it, and writes it atomically (temp file +
/// rename) to `out_path`.
///
/// # Errors
///
/// Returns an error if the database copy, compression, or file write fails.
pub fn export_snapshot(
    db: &Database,
    project_root: &Path,
    out_path: &Path,
) -> Result<ExportReport> {
    let project_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let pattern = project_prefix_pattern(&project_root);

    let tmp_dir = tempfile::tempdir()?;
    let snap_path = tmp_dir.path().join("snapshot.db");
    let snap_path_str = snap_path.to_string_lossy().to_string();
    let project_root_str = project_root.to_string_lossy().to_string();

    let report_counts = db.with_conn(|conn| {
        conn.execute("ATTACH DATABASE ?1 AS snap", [&snap_path_str])
            .map_err(|e| db_err("failed to attach snapshot db", &e))?;

        let result = build_snapshot_db(conn, &pattern, &project_root_str);

        // Always detach, even on failure.
        let detach = conn.execute("DETACH DATABASE snap", []);
        let counts = result?;
        detach.map_err(|e| db_err("failed to detach snapshot db", &e))?;
        Ok(counts)
    })?;

    // Compress and write atomically.
    let raw = std::fs::read(&snap_path)?;
    let compressed = zstd::encode_all(raw.as_slice(), ZSTD_LEVEL)
        .map_err(|e| Error::internal(format!("zstd compression failed: {e}")))?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out_dir = out_path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_out = tempfile::NamedTempFile::new_in(out_dir)?;
    std::fs::write(tmp_out.path(), &compressed)?;
    tmp_out
        .persist(out_path)
        .map_err(|e| Error::internal(format!("failed to persist snapshot: {e}")))?;

    let report = ExportReport {
        project_root: project_root_str,
        out_path: out_path.to_string_lossy().to_string(),
        files: report_counts.files,
        chunks: report_counts.chunks,
        embeddings: report_counts.embeddings,
        symbols: report_counts.symbols,
        structural_edges: report_counts.structural_edges,
        graph_nodes: report_counts.graph_nodes,
        graph_edges: report_counts.graph_edges,
        raw_bytes: raw.len() as u64,
        compressed_bytes: compressed.len() as u64,
    };

    tracing::info!(
        out = %report.out_path,
        files = report.files,
        chunks = report.chunks,
        raw_bytes = report.raw_bytes,
        compressed_bytes = report.compressed_bytes,
        "Snapshot exported"
    );

    Ok(report)
}

/// Table counts collected while building the snapshot DB.
struct SnapshotCounts {
    files: u64,
    chunks: u64,
    embeddings: u64,
    symbols: u64,
    structural_edges: u64,
    graph_nodes: u64,
    graph_edges: u64,
}

/// Create the snapshot schema in the attached `snap` database and copy the
/// project-scoped rows into it.
fn build_snapshot_db(
    conn: &Connection,
    pattern: &str,
    project_root: &str,
) -> Result<SnapshotCounts> {
    // Index-only schema. `chunk_embeddings` is a PLAIN table here (not vec0)
    // so the artifact is readable without the sqlite-vec extension; the
    // importer copies blobs back into the local vec0 table.
    conn.execute_batch(
        r"
        CREATE TABLE snap.snapshot_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE snap.schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        );
        CREATE TABLE snap.chunks (
            id INTEGER PRIMARY KEY,
            file_path TEXT NOT NULL,
            chunk_index INTEGER NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            content TEXT NOT NULL,
            language TEXT,
            file_hash TEXT NOT NULL,
            indexed_at INTEGER NOT NULL,
            UNIQUE(file_path, chunk_index)
        );
        CREATE TABLE snap.chunk_embeddings (
            id INTEGER PRIMARY KEY,
            embedding BLOB NOT NULL
        );
        CREATE TABLE snap.symbols (
            id INTEGER PRIMARY KEY,
            file_path TEXT NOT NULL,
            symbol_name TEXT NOT NULL,
            symbol_kind TEXT NOT NULL,
            language TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            scope TEXT,
            signature TEXT,
            file_hash TEXT NOT NULL,
            indexed_at INTEGER NOT NULL
        );
        CREATE TABLE snap.structural_edges (
            id INTEGER PRIMARY KEY,
            source_symbol_id INTEGER NOT NULL,
            target_symbol_name TEXT NOT NULL,
            target_file_path TEXT,
            edge_kind TEXT NOT NULL,
            indexed_at INTEGER NOT NULL
        );
        CREATE TABLE snap.graph_nodes (
            id TEXT PRIMARY KEY,
            node_type TEXT NOT NULL,
            label TEXT NOT NULL,
            label_normalized TEXT NOT NULL,
            record_id TEXT,
            metadata TEXT,
            created_at INTEGER NOT NULL,
            last_accessed INTEGER NOT NULL,
            access_count INTEGER DEFAULT 0
        );
        CREATE TABLE snap.graph_edges (
            id TEXT PRIMARY KEY,
            from_node TEXT NOT NULL,
            to_node TEXT NOT NULL,
            relationship TEXT NOT NULL,
            confidence REAL DEFAULT 0.3,
            provisional INTEGER DEFAULT 1,
            context TEXT,
            created_at INTEGER NOT NULL,
            last_confirmed INTEGER NOT NULL,
            access_count INTEGER DEFAULT 0,
            success_count INTEGER DEFAULT 0,
            failure_count INTEGER DEFAULT 0
        );
        CREATE TABLE snap.file_state (
            path TEXT PRIMARY KEY,
            mtime INTEGER NOT NULL,
            size INTEGER NOT NULL,
            hash TEXT NOT NULL,
            last_indexed INTEGER NOT NULL
        );
        ",
    )
    .map_err(|e| db_err("failed to create snapshot schema", &e))?;

    // Copy project-scoped rows.
    conn.execute(
        "INSERT INTO snap.schema_migrations SELECT version, applied_at FROM schema_migrations",
        [],
    )
    .map_err(|e| db_err("failed to copy schema_migrations", &e))?;

    let files = conn
        .execute(
            "INSERT INTO snap.file_state
             SELECT path, mtime, size, hash, last_indexed FROM file_state WHERE path LIKE ?1",
            [pattern],
        )
        .map_err(|e| db_err("failed to copy file_state", &e))? as u64;

    let chunks = conn
        .execute(
            "INSERT INTO snap.chunks
             SELECT id, file_path, chunk_index, start_line, end_line, content, language,
                    file_hash, indexed_at
             FROM chunks WHERE file_path LIKE ?1",
            [pattern],
        )
        .map_err(|e| db_err("failed to copy chunks", &e))? as u64;

    // Embeddings travel with the snapshot when the vec0 table exists.
    let embeddings = if table_exists(conn, "chunk_embeddings") {
        conn.execute(
            "INSERT INTO snap.chunk_embeddings (id, embedding)
             SELECT ce.id, ce.embedding
             FROM chunk_embeddings ce
             JOIN chunks c ON c.id = ce.id
             WHERE c.file_path LIKE ?1",
            [pattern],
        )
        .map_err(|e| db_err("failed to copy chunk embeddings", &e))? as u64
    } else {
        0
    };

    let symbols = conn
        .execute(
            "INSERT INTO snap.symbols
             SELECT id, file_path, symbol_name, symbol_kind, language, start_line, end_line,
                    scope, signature, file_hash, indexed_at
             FROM symbols WHERE file_path LIKE ?1",
            [pattern],
        )
        .map_err(|e| db_err("failed to copy symbols", &e))? as u64;

    let structural_edges =
        conn.execute(
            "INSERT INTO snap.structural_edges
             SELECT se.id, se.source_symbol_id, se.target_symbol_name, se.target_file_path,
                    se.edge_kind, se.indexed_at
             FROM structural_edges se
             JOIN symbols s ON s.id = se.source_symbol_id
             WHERE s.file_path LIKE ?1",
            [pattern],
        )
        .map_err(|e| db_err("failed to copy structural_edges", &e))? as u64;

    let graph_nodes = conn
        .execute("INSERT INTO snap.graph_nodes SELECT * FROM graph_nodes", [])
        .map_err(|e| db_err("failed to copy graph_nodes", &e))? as u64;

    let graph_edges = conn
        .execute("INSERT INTO snap.graph_edges SELECT * FROM graph_edges", [])
        .map_err(|e| db_err("failed to copy graph_edges", &e))? as u64;

    // Manifest for compatibility checks on import.
    let now = chrono::Utc::now().to_rfc3339();
    let meta: [(&str, String); 5] = [
        ("schema_version", SCHEMA_VERSION.to_string()),
        ("embedding_dim", EMBEDDING_DIM.to_string()),
        ("nellie_version", env!("CARGO_PKG_VERSION").to_string()),
        ("created_at", now),
        ("project_root", project_root.to_string()),
    ];
    for (key, value) in &meta {
        conn.execute(
            "INSERT INTO snap.snapshot_meta (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .map_err(|e| db_err("failed to write snapshot_meta", &e))?;
    }

    Ok(SnapshotCounts {
        files,
        chunks,
        embeddings,
        symbols,
        structural_edges,
        graph_nodes,
        graph_edges,
    })
}

/// Check whether a table (or virtual table) exists in the main database.
fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE name = ?1",
        [name],
        |_| Ok(true),
    )
    .unwrap_or(false)
}

/// Import a snapshot into the local database.
///
/// Verifies the snapshot's schema version and embedding dimension against
/// this build and refuses loudly on mismatch. Merges per-file: files whose
/// hash already matches the local index are skipped; otherwise local rows
/// for the file are replaced with the snapshot's rows. Semantic graph
/// nodes/edges merge with `INSERT OR IGNORE` (local rows win).
///
/// The caller should run `diff_index` over the project root afterwards so
/// files changed since the snapshot are reindexed and deletions pruned.
///
/// # Errors
///
/// Returns an error if the snapshot is unreadable, incompatible, or the
/// merge fails (the local database is rolled back on failure).
pub fn import_snapshot(db: &Database, in_path: &Path) -> Result<ImportReport> {
    let compressed = std::fs::read(in_path)
        .map_err(|e| Error::internal(format!("cannot read snapshot {}: {e}", in_path.display())))?;
    let raw = zstd::decode_all(compressed.as_slice()).map_err(|e| {
        Error::internal(format!(
            "cannot decompress snapshot {} (is it a zstd file?): {e}",
            in_path.display()
        ))
    })?;

    let tmp_dir = tempfile::tempdir()?;
    let snap_path = tmp_dir.path().join("snapshot.db");
    std::fs::write(&snap_path, &raw)?;

    verify_snapshot_compat(&snap_path)?;

    let snap_path_str = snap_path.to_string_lossy().to_string();
    let mut report = db.with_conn(|conn| {
        conn.execute("ATTACH DATABASE ?1 AS snap", [&snap_path_str])
            .map_err(|e| db_err("failed to attach snapshot db", &e))?;

        // Manual transaction so the merge is atomic while `snap` is attached
        // (ATTACH itself cannot run inside a transaction).
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db_err("failed to begin import transaction", &e))?;

        let merge_result = merge_snapshot(conn);

        let result = match merge_result {
            Ok(rep) => conn
                .execute_batch("COMMIT")
                .map_err(|e| db_err("failed to commit import", &e))
                .map(|()| rep),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        };

        let _ = conn.execute("DETACH DATABASE snap", []);
        result
    })?;

    report.in_path = in_path.to_string_lossy().to_string();
    tracing::info!(
        input = %report.in_path,
        files_merged = report.files_merged,
        files_skipped = report.files_skipped,
        chunks = report.chunks_inserted,
        embeddings = report.embeddings_inserted,
        "Snapshot imported"
    );
    Ok(report)
}

/// Verify snapshot schema version and embedding dimension match this build.
fn verify_snapshot_compat(snap_path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(snap_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| db_err("failed to open snapshot db", &e))?;

    let get_meta = |key: &str| -> Result<String> {
        conn.query_row(
            "SELECT value FROM snapshot_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| {
            Error::internal(format!(
                "not a valid nellie snapshot (missing snapshot_meta '{key}'): {e}"
            ))
        })
    };

    let snap_schema: i32 = get_meta("schema_version")?
        .parse()
        .map_err(|e| Error::internal(format!("snapshot has invalid schema_version: {e}")))?;
    if snap_schema != SCHEMA_VERSION {
        return Err(Error::internal(format!(
            "snapshot schema version mismatch: snapshot is v{snap_schema}, this nellie build \
             uses v{SCHEMA_VERSION}. Re-export the snapshot with a matching nellie version. \
             Nothing was imported."
        )));
    }

    let snap_dim: usize = get_meta("embedding_dim")?
        .parse()
        .map_err(|e| Error::internal(format!("snapshot has invalid embedding_dim: {e}")))?;
    if snap_dim != EMBEDDING_DIM {
        return Err(Error::internal(format!(
            "snapshot embedding dimension mismatch: snapshot has {snap_dim}-dim vectors, this \
             nellie build uses {EMBEDDING_DIM}-dim. Re-export the snapshot with a matching \
             embedding model. Nothing was imported."
        )));
    }

    Ok(())
}

/// Merge the attached `snap` database into the local one.
#[allow(clippy::too_many_lines)]
fn merge_snapshot(conn: &Connection) -> Result<ImportReport> {
    let mut report = ImportReport {
        in_path: String::new(),
        files_merged: 0,
        files_skipped: 0,
        chunks_inserted: 0,
        embeddings_inserted: 0,
        symbols_inserted: 0,
        structural_edges_inserted: 0,
        graph_nodes_merged: 0,
        graph_edges_merged: 0,
    };

    let snapshot_has_embeddings = table_exists(conn, "chunk_embeddings");

    // Collect the snapshot's file list up front.
    let snap_files: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT path, hash FROM snap.file_state ORDER BY path")
            .map_err(|e| db_err("failed to read snapshot file_state", &e))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| db_err("failed to read snapshot file_state", &e))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| db_err("failed to read snapshot file_state", &e))?
    };

    for (path, snap_hash) in &snap_files {
        // Skip files the local index already has with an identical hash.
        let local_hash: Option<String> = conn
            .query_row(
                "SELECT hash FROM file_state WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .map_or_else(
                |e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(db_err("failed to read local file_state", &other)),
                },
                |h| Ok(Some(h)),
            )?;

        if local_hash.as_deref() == Some(snap_hash.as_str()) {
            report.files_skipped += 1;
            continue;
        }

        // Replace local rows for this file with the snapshot's rows.
        crate::storage::delete_chunks_by_file(conn, path)?;
        conn.execute("DELETE FROM symbols WHERE file_path = ?1", [path])
            .map_err(|e| db_err("failed to clear local symbols", &e))?;

        merge_file_chunks(conn, path, snapshot_has_embeddings, &mut report)?;
        merge_file_symbols(conn, path, &mut report)?;

        conn.execute(
            "INSERT OR REPLACE INTO file_state (path, mtime, size, hash, last_indexed)
             SELECT path, mtime, size, hash, last_indexed FROM snap.file_state WHERE path = ?1",
            [path],
        )
        .map_err(|e| db_err("failed to merge file_state", &e))?;

        report.files_merged += 1;
    }

    // Semantic graph: keep local rows on conflict.
    report.graph_nodes_merged =
        conn.execute(
            "INSERT OR IGNORE INTO graph_nodes SELECT * FROM snap.graph_nodes",
            [],
        )
        .map_err(|e| db_err("failed to merge graph_nodes", &e))? as u64;
    report.graph_edges_merged =
        conn.execute(
            "INSERT OR IGNORE INTO graph_edges SELECT * FROM snap.graph_edges",
            [],
        )
        .map_err(|e| db_err("failed to merge graph_edges", &e))? as u64;

    Ok(report)
}

/// Copy one file's chunks (and embeddings) from the snapshot into the local
/// database, remapping chunk IDs to freshly assigned local rowids.
fn merge_file_chunks(
    conn: &Connection,
    path: &str,
    snapshot_has_embeddings: bool,
    report: &mut ImportReport,
) -> Result<()> {
    struct SnapChunk {
        old_id: i64,
        chunk_index: i32,
        start_line: i32,
        end_line: i32,
        content: String,
        language: Option<String>,
        file_hash: String,
        indexed_at: i64,
    }

    let chunks: Vec<SnapChunk> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, chunk_index, start_line, end_line, content, language, file_hash,
                        indexed_at
                 FROM snap.chunks WHERE file_path = ?1 ORDER BY chunk_index",
            )
            .map_err(|e| db_err("failed to read snapshot chunks", &e))?;
        let rows = stmt
            .query_map([path], |row| {
                Ok(SnapChunk {
                    old_id: row.get(0)?,
                    chunk_index: row.get(1)?,
                    start_line: row.get(2)?,
                    end_line: row.get(3)?,
                    content: row.get(4)?,
                    language: row.get(5)?,
                    file_hash: row.get(6)?,
                    indexed_at: row.get(7)?,
                })
            })
            .map_err(|e| db_err("failed to read snapshot chunks", &e))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| db_err("failed to read snapshot chunks", &e))?
    };

    let local_has_vec_table = table_exists(conn, "chunk_embeddings");

    for chunk in chunks {
        conn.execute(
            "INSERT INTO chunks (file_path, chunk_index, start_line, end_line, content,
                                 language, file_hash, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                path,
                chunk.chunk_index,
                chunk.start_line,
                chunk.end_line,
                chunk.content,
                chunk.language,
                chunk.file_hash,
                chunk.indexed_at,
            ],
        )
        .map_err(|e| db_err("failed to insert chunk", &e))?;
        let new_id = conn.last_insert_rowid();
        report.chunks_inserted += 1;

        if snapshot_has_embeddings && local_has_vec_table {
            let blob: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT embedding FROM snap.chunk_embeddings WHERE id = ?1",
                    [chunk.old_id],
                    |row| row.get(0),
                )
                .map_or_else(
                    |e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(db_err("failed to read snapshot embedding", &other)),
                    },
                    |b| Ok(Some(b)),
                )?;

            if let Some(blob) = blob {
                if blob.len() == EMBEDDING_DIM * 4 {
                    conn.execute(
                        "INSERT INTO chunk_embeddings (id, embedding) VALUES (?1, ?2)",
                        rusqlite::params![new_id, blob],
                    )
                    .map_err(|e| db_err("failed to insert embedding", &e))?;
                    report.embeddings_inserted += 1;
                } else {
                    tracing::warn!(
                        path,
                        chunk = chunk.old_id,
                        bytes = blob.len(),
                        "Skipping embedding with unexpected byte length"
                    );
                }
            }
        }
    }

    Ok(())
}

/// Copy one file's symbols and their structural edges from the snapshot,
/// remapping symbol IDs to freshly assigned local rowids.
fn merge_file_symbols(conn: &Connection, path: &str, report: &mut ImportReport) -> Result<()> {
    struct SnapSymbol {
        old_id: i64,
        symbol_name: String,
        symbol_kind: String,
        language: String,
        start_line: i64,
        end_line: i64,
        scope: Option<String>,
        signature: Option<String>,
        file_hash: String,
        indexed_at: i64,
    }

    let symbols: Vec<SnapSymbol> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, symbol_name, symbol_kind, language, start_line, end_line, scope,
                        signature, file_hash, indexed_at
                 FROM snap.symbols WHERE file_path = ?1 ORDER BY id",
            )
            .map_err(|e| db_err("failed to read snapshot symbols", &e))?;
        let rows = stmt
            .query_map([path], |row| {
                Ok(SnapSymbol {
                    old_id: row.get(0)?,
                    symbol_name: row.get(1)?,
                    symbol_kind: row.get(2)?,
                    language: row.get(3)?,
                    start_line: row.get(4)?,
                    end_line: row.get(5)?,
                    scope: row.get(6)?,
                    signature: row.get(7)?,
                    file_hash: row.get(8)?,
                    indexed_at: row.get(9)?,
                })
            })
            .map_err(|e| db_err("failed to read snapshot symbols", &e))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| db_err("failed to read snapshot symbols", &e))?
    };

    for symbol in symbols {
        conn.execute(
            "INSERT INTO symbols (file_path, symbol_name, symbol_kind, language, start_line,
                                  end_line, scope, signature, file_hash, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                path,
                symbol.symbol_name,
                symbol.symbol_kind,
                symbol.language,
                symbol.start_line,
                symbol.end_line,
                symbol.scope,
                symbol.signature,
                symbol.file_hash,
                symbol.indexed_at,
            ],
        )
        .map_err(|e| db_err("failed to insert symbol", &e))?;
        let new_id = conn.last_insert_rowid();
        report.symbols_inserted += 1;

        let edges = conn
            .execute(
                "INSERT INTO structural_edges (source_symbol_id, target_symbol_name,
                                               target_file_path, edge_kind, indexed_at)
                 SELECT ?1, target_symbol_name, target_file_path, edge_kind, indexed_at
                 FROM snap.structural_edges WHERE source_symbol_id = ?2",
                rusqlite::params![new_id, symbol.old_id],
            )
            .map_err(|e| db_err("failed to insert structural edges", &e))?;
        report.structural_edges_inserted += edges as u64;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::init_storage;

    fn setup_db(tmp: &tempfile::TempDir, name: &str) -> Database {
        let db = Database::open(tmp.path().join(name)).unwrap();
        init_storage(&db).unwrap();
        db
    }

    fn insert_test_file(db: &Database, path: &str, hash: &str, with_embedding: bool) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO chunks (file_path, chunk_index, start_line, end_line, content,
                                     language, file_hash, indexed_at)
                 VALUES (?1, 0, 1, 10, 'fn main() {}', 'rust', ?2, 1000)",
                rusqlite::params![path, hash],
            )
            .unwrap();
            let id = conn.last_insert_rowid();
            if with_embedding {
                let embedding: Vec<f32> = (0..EMBEDDING_DIM).map(|i| i as f32 * 0.01).collect();
                crate::storage::insert_vector(conn, "chunk_embeddings", id, &embedding).unwrap();
            }
            conn.execute(
                "INSERT OR REPLACE INTO file_state (path, mtime, size, hash, last_indexed)
                 VALUES (?1, 1000, 12, ?2, 1000)",
                rusqlite::params![path, hash],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_export_import_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let file_path = project.join("main.rs");
        let file_path_str = file_path.to_string_lossy().to_string();

        let db = setup_db(&tmp, "source.db");
        insert_test_file(&db, &file_path_str, "hash-a", true);
        // A file OUTSIDE the project must not travel with the snapshot.
        insert_test_file(&db, "/elsewhere/other.rs", "hash-x", false);

        let out = tmp.path().join("snap/graph.db.zst");
        let report = export_snapshot(&db, &project, &out).unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(report.chunks, 1);
        assert_eq!(report.embeddings, 1);
        assert!(out.exists());
        assert!(
            report.compressed_bytes < report.raw_bytes,
            "compression should shrink the artifact ({} vs {})",
            report.compressed_bytes,
            report.raw_bytes
        );

        // Import into a fresh database.
        let db2 = setup_db(&tmp, "dest.db");
        let import = import_snapshot(&db2, &out).unwrap();
        assert_eq!(import.files_merged, 1);
        assert_eq!(import.files_skipped, 0);
        assert_eq!(import.chunks_inserted, 1);
        assert_eq!(import.embeddings_inserted, 1);

        // Chunk content survives the round trip.
        let content: String = db2
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT content FROM chunks WHERE file_path = ?1",
                    [&file_path_str],
                    |row| row.get(0),
                )
                .map_err(|e| crate::error::StorageError::Database(e.to_string()).into())
            })
            .unwrap();
        assert_eq!(content, "fn main() {}");

        // The out-of-project file did not travel.
        let stray: i64 = db2
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM chunks WHERE file_path = '/elsewhere/other.rs'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| crate::error::StorageError::Database(e.to_string()).into())
            })
            .unwrap();
        assert_eq!(stray, 0);

        // Embedding blob is intact (correct byte length for the dim).
        let emb_count: i64 = db2
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM chunk_embeddings", [], |row| {
                    row.get(0)
                })
                .map_err(|e| crate::error::StorageError::Database(e.to_string()).into())
            })
            .unwrap();
        assert_eq!(emb_count, 1);
    }

    #[test]
    fn test_import_skips_identical_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let file_path = project.join("lib.rs").to_string_lossy().to_string();

        let db = setup_db(&tmp, "source.db");
        insert_test_file(&db, &file_path, "same-hash", false);

        let out = tmp.path().join("graph.db.zst");
        export_snapshot(&db, &project, &out).unwrap();

        // Destination already has the file with the same hash.
        let db2 = setup_db(&tmp, "dest.db");
        insert_test_file(&db2, &file_path, "same-hash", false);

        let import = import_snapshot(&db2, &out).unwrap();
        assert_eq!(import.files_merged, 0);
        assert_eq!(import.files_skipped, 1);
        assert_eq!(import.chunks_inserted, 0);
    }

    #[test]
    fn test_import_replaces_stale_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let file_path = project.join("lib.rs").to_string_lossy().to_string();

        let db = setup_db(&tmp, "source.db");
        insert_test_file(&db, &file_path, "new-hash", false);
        let out = tmp.path().join("graph.db.zst");
        export_snapshot(&db, &project, &out).unwrap();

        // Destination has an OLDER version of the file.
        let db2 = setup_db(&tmp, "dest.db");
        insert_test_file(&db2, &file_path, "old-hash", false);

        let import = import_snapshot(&db2, &out).unwrap();
        assert_eq!(import.files_merged, 1);

        let hash: String = db2
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT hash FROM file_state WHERE path = ?1",
                    [&file_path],
                    |row| row.get(0),
                )
                .map_err(|e| crate::error::StorageError::Database(e.to_string()).into())
            })
            .unwrap();
        assert_eq!(hash, "new-hash");
    }

    #[test]
    fn test_import_refuses_schema_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let db = setup_db(&tmp, "source.db");
        let out = tmp.path().join("graph.db.zst");
        export_snapshot(&db, &project, &out).unwrap();

        // Tamper: rewrite the snapshot with a bogus schema version.
        let raw = zstd::decode_all(std::fs::read(&out).unwrap().as_slice()).unwrap();
        let tampered_db = tmp.path().join("tampered.db");
        std::fs::write(&tampered_db, raw).unwrap();
        {
            let conn = Connection::open(&tampered_db).unwrap();
            conn.execute(
                "UPDATE snapshot_meta SET value = '999' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        let tampered = tmp.path().join("tampered.db.zst");
        let recompressed =
            zstd::encode_all(std::fs::read(&tampered_db).unwrap().as_slice(), 3).unwrap();
        std::fs::write(&tampered, recompressed).unwrap();

        let db2 = setup_db(&tmp, "dest.db");
        let err = import_snapshot(&db2, &tampered).unwrap_err();
        assert!(
            err.to_string().contains("schema version mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_import_refuses_dim_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let db = setup_db(&tmp, "source.db");
        let out = tmp.path().join("graph.db.zst");
        export_snapshot(&db, &project, &out).unwrap();

        let raw = zstd::decode_all(std::fs::read(&out).unwrap().as_slice()).unwrap();
        let tampered_db = tmp.path().join("tampered.db");
        std::fs::write(&tampered_db, raw).unwrap();
        {
            let conn = Connection::open(&tampered_db).unwrap();
            conn.execute(
                "UPDATE snapshot_meta SET value = '768' WHERE key = 'embedding_dim'",
                [],
            )
            .unwrap();
        }
        let tampered = tmp.path().join("tampered.db.zst");
        let recompressed =
            zstd::encode_all(std::fs::read(&tampered_db).unwrap().as_slice(), 3).unwrap();
        std::fs::write(&tampered, recompressed).unwrap();

        let db2 = setup_db(&tmp, "dest.db");
        let err = import_snapshot(&db2, &tampered).unwrap_err();
        assert!(
            err.to_string().contains("embedding dimension mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_import_rejects_garbage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let garbage = tmp.path().join("garbage.zst");
        std::fs::write(&garbage, b"not a zstd file at all").unwrap();

        let db = setup_db(&tmp, "dest.db");
        let err = import_snapshot(&db, &garbage).unwrap_err();
        assert!(err.to_string().contains("decompress"), "got: {err}");
    }

    #[test]
    fn test_default_snapshot_path() {
        assert_eq!(
            default_snapshot_path(Path::new("/repo")),
            PathBuf::from("/repo/.nellie/graph.db.zst")
        );
    }
}
