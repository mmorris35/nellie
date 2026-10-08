//! Embedding index metadata: which spec built the active vector tables.
//!
//! Vectors embedded under different settings must never be mixed, so the
//! `embedding_meta` table records the [`EmbeddingSpec`] and the vector table
//! names that are currently active. Every read and write of a vector table
//! resolves its name through [`active_tables`], and processes that write
//! embeddings refuse to start when the recorded spec differs from
//! [`EmbeddingSpec::current`] (see [`check_spec`]).
//!
//! Table names are never taken from user input: whatever is stored in
//! `embedding_meta` is matched against the fixed names defined in this module,
//! and anything else is rejected.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::vector::EMBEDDING_DIM;
use crate::embeddings::EmbeddingSpec;
use crate::error::StorageError;
use crate::Result;

/// Names of the vector tables for chunks, lessons and checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorTables {
    /// Code chunk vectors (`id INTEGER`).
    pub chunks: &'static str,
    /// Lesson vectors (`id TEXT`).
    pub lessons: &'static str,
    /// Checkpoint vectors (`id TEXT`).
    pub checkpoints: &'static str,
}

impl VectorTables {
    /// All three table names.
    #[must_use]
    pub const fn all(&self) -> [&'static str; 3] {
        [self.chunks, self.lessons, self.checkpoints]
    }
}

/// Tables written by Nellie versions that inherited truncation from
/// `tokenizer.json`.
pub const LEGACY_TABLES: VectorTables = VectorTables {
    chunks: "chunk_embeddings",
    lessons: "lesson_embeddings",
    checkpoints: "checkpoint_embeddings",
};

/// Tables for [`EmbeddingSpec::current`] (all-MiniLM-L6-v2, 256 tokens).
pub const CURRENT_TABLES: VectorTables = VectorTables {
    chunks: "chunk_embeddings_minilm256",
    lessons: "lesson_embeddings_minilm256",
    checkpoints: "checkpoint_embeddings_minilm256",
};

/// Every table set this binary knows. Names read from the database must be
/// one of these.
pub const KNOWN_TABLE_SETS: [VectorTables; 2] = [LEGACY_TABLES, CURRENT_TABLES];

/// The active row of `embedding_meta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveMeta {
    /// Row id in `embedding_meta`.
    pub id: i64,
    /// Spec the active tables were built with.
    pub spec: EmbeddingSpec,
    /// Active vector table names.
    pub tables: VectorTables,
    /// How this row was recorded (`fresh`, `pre-guard`, `reembed`).
    pub source: String,
}

/// Map a stored table name to the matching compiled-in name.
fn resolve_name(stored: &str, pick: fn(&VectorTables) -> &'static str) -> Result<&'static str> {
    KNOWN_TABLE_SETS
        .iter()
        .map(pick)
        .find(|name| *name == stored)
        .ok_or_else(|| {
            StorageError::Vector(format!(
                "embedding_meta names unknown vector table '{stored}'; \
                 this database was written by a newer Nellie"
            ))
            .into()
        })
}

/// Read the active metadata row, if any.
///
/// # Errors
///
/// Returns an error if the query fails or a stored table name is unknown.
pub fn active_meta(conn: &Connection) -> Result<Option<ActiveMeta>> {
    let row = conn
        .query_row(
            "SELECT id, model_id, dim, max_len, special_tokens, query_prompt, doc_prompt,
                    normalisation, chunk_table, lesson_table, checkpoint_table, source
             FROM embedding_meta WHERE active = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    EmbeddingSpec {
                        model_id: row.get(1)?,
                        dim: usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                        max_len: usize::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                        special_tokens: row.get(4)?,
                        query_prompt: row.get(5)?,
                        doc_prompt: row.get(6)?,
                        normalisation: row.get(7)?,
                    },
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                ))
            },
        )
        .optional()
        .map_err(|e| StorageError::Database(format!("failed to read embedding_meta: {e}")))?;

    let Some((id, spec, chunks, lessons, checkpoints, source)) = row else {
        return Ok(None);
    };

    Ok(Some(ActiveMeta {
        id,
        spec,
        tables: VectorTables {
            chunks: resolve_name(&chunks, |t| t.chunks)?,
            lessons: resolve_name(&lessons, |t| t.lessons)?,
            checkpoints: resolve_name(&checkpoints, |t| t.checkpoints)?,
        },
        source,
    }))
}

/// Names of the vector tables reads and writes must use.
///
/// # Errors
///
/// Returns an error if no spec has been recorded yet (an install that predates
/// the embedding guard and has not been checked by a server start or
/// `nellie reembed`).
pub fn active_tables(conn: &Connection) -> Result<VectorTables> {
    active_meta(conn)?.map(|m| m.tables).ok_or_else(|| {
        StorageError::Vector(
            "embedding index metadata is missing; start the Nellie server or run \
             `nellie reembed` to initialise it"
                .to_string(),
        )
        .into()
    })
}

/// Does a table with this name exist?
pub(crate) fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?)",
        [name],
        |row| row.get(0),
    )
    .map_err(|e| StorageError::Database(format!("failed to check table {name}: {e}")).into())
}

/// True when no known vector table contains any rows (a fresh install).
fn all_vector_tables_empty(conn: &Connection) -> Result<bool> {
    for set in &KNOWN_TABLE_SETS {
        for name in set.all() {
            if !table_exists(conn, name)? {
                continue;
            }
            let has_rows: bool = conn
                .query_row(&format!("SELECT EXISTS(SELECT 1 FROM {name})"), [], |row| {
                    row.get(0)
                })
                .map_err(|e| StorageError::Database(format!("failed to inspect {name}: {e}")))?;
            if has_rows {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Create the vec0 tables of a table set if they do not exist.
///
/// # Errors
///
/// Returns an error if a table cannot be created.
pub fn create_vector_tables(conn: &Connection, tables: &VectorTables) -> Result<()> {
    for (name, id_type) in [
        (tables.chunks, "INTEGER"),
        (tables.lessons, "TEXT"),
        (tables.checkpoints, "TEXT"),
    ] {
        conn.execute(
            &format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS {name} USING vec0(
                    id {id_type} PRIMARY KEY,
                    embedding FLOAT[{EMBEDDING_DIM}]
                )"
            ),
            [],
        )
        .map_err(|e| StorageError::Vector(format!("failed to create {name}: {e}")))?;
    }
    Ok(())
}

/// Make `spec`/`tables` the active row. Must run inside a transaction.
///
/// # Errors
///
/// Returns an error if the update fails.
pub fn record_active(
    conn: &Connection,
    spec: &EmbeddingSpec,
    tables: &VectorTables,
    source: &str,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    conn.execute("UPDATE embedding_meta SET active = 0 WHERE active = 1", [])
        .map_err(|e| StorageError::Database(format!("failed to update embedding_meta: {e}")))?;
    conn.execute(
        "INSERT INTO embedding_meta (model_id, dim, max_len, special_tokens, query_prompt,
             doc_prompt, normalisation, chunk_table, lesson_table, checkpoint_table, active,
             source, recorded_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
        params![
            spec.model_id,
            i64::try_from(spec.dim).unwrap_or(i64::MAX),
            i64::try_from(spec.max_len).unwrap_or(i64::MAX),
            spec.special_tokens,
            spec.query_prompt,
            spec.doc_prompt,
            spec.normalisation,
            tables.chunks,
            tables.lessons,
            tables.checkpoints,
            source,
            now,
        ],
    )
    .map_err(|e| StorageError::Database(format!("failed to write embedding_meta: {e}")))?;
    Ok(())
}

fn begin_immediate(conn: &Connection) -> Result<Transaction<'_>> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| StorageError::Database(format!("failed to begin transaction: {e}")).into())
}

fn commit(tx: Transaction<'_>) -> Result<()> {
    tx.commit()
        .map_err(|e| StorageError::Database(format!("failed to commit: {e}")).into())
}

fn meta_is_empty(conn: &Connection) -> Result<bool> {
    conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM embedding_meta)", [], |r| {
        r.get(0)
    })
    .map_err(|e| StorageError::Database(format!("failed to read embedding_meta: {e}")).into())
}

/// Ensure the active vector tables exist, recording the current spec first
/// if this is a fresh install (no metadata and no stored vectors).
///
/// On an install that predates the embedding guard this does nothing; the
/// spec it was built with is recorded by [`check_spec`], which needs the
/// install's `tokenizer.json`.
///
/// # Errors
///
/// Returns an error if the metadata cannot be read or written.
pub fn ensure_vector_tables(conn: &Connection) -> Result<()> {
    let tx = begin_immediate(conn)?;
    if meta_is_empty(&tx)? && all_vector_tables_empty(&tx)? {
        record_active(&tx, &EmbeddingSpec::current(), &CURRENT_TABLES, "fresh")?;
        tracing::info!(
            "Fresh install: recorded embedding spec {}",
            EmbeddingSpec::current()
        );
    }
    if let Some(meta) = active_meta(&tx)? {
        create_vector_tables(&tx, &meta.tables)?;
    }
    commit(tx)
}

/// Record the spec of an install that predates the embedding guard.
///
/// Reads the install's own `tokenizer.json` to find what was actually used,
/// and records it with the legacy table names. Written once: does nothing if
/// any metadata already exists.
///
/// # Errors
///
/// Returns an error if the tokenizer file cannot be read or the metadata
/// cannot be written.
pub fn bootstrap_meta(conn: &Connection, tokenizer_path: &Path) -> Result<()> {
    let tx = begin_immediate(conn)?;
    if !meta_is_empty(&tx)? {
        return commit(tx);
    }
    if all_vector_tables_empty(&tx)? {
        record_active(&tx, &EmbeddingSpec::current(), &CURRENT_TABLES, "fresh")?;
        create_vector_tables(&tx, &CURRENT_TABLES)?;
        return commit(tx);
    }
    let spec = EmbeddingSpec::legacy_from_tokenizer_json(tokenizer_path).map_err(|e| {
        StorageError::Vector(format!(
            "this index was built by an earlier Nellie and the tokenizer it used is needed \
             to identify its settings: {e}"
        ))
    })?;
    record_active(&tx, &spec, &LEGACY_TABLES, "pre-guard")?;
    tracing::info!("Recorded embedding spec of existing index: {spec}");
    commit(tx)
}

/// Message shown when the index spec differs from this binary's.
#[must_use]
pub fn mismatch_message(recorded: &EmbeddingSpec, current: &EmbeddingSpec) -> String {
    format!(
        "Nellie will not start: the vector index was built with a different embedding spec.\n\
         \n  index built with:   {recorded}\
         \n  this version uses:  {current}\n\
         \n\
         Mixing the two would silently degrade search. Stop Nellie and run\n\
         \n    nellie reembed\n\
         \n\
         once to rebuild the index (search quality improves). The old index is kept for rollback."
    )
}

/// Bootstrap the metadata if needed, then compare the recorded spec with
/// [`EmbeddingSpec::current`].
///
/// Returns `Ok(Err(message))` when the specs differ; the caller must refuse to
/// run.
///
/// # Errors
///
/// Returns an error if the metadata cannot be read or bootstrapped.
pub fn check_spec(
    conn: &Connection,
    tokenizer_path: &Path,
) -> Result<std::result::Result<(), String>> {
    bootstrap_meta(conn, tokenizer_path)?;
    let meta = active_meta(conn)?
        .ok_or_else(|| StorageError::Vector("embedding_meta has no active row".to_string()))?;
    let current = EmbeddingSpec::current();
    if meta.spec == current {
        create_vector_tables(conn, &meta.tables)?;
        Ok(Ok(()))
    } else {
        Ok(Err(mismatch_message(&meta.spec, &current)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{init_storage, migrate, Database};

    fn tokenizer_json(dir: &Path, truncation: &str) -> std::path::PathBuf {
        let path = dir.join("tokenizer.json");
        std::fs::write(&path, format!(r#"{{"truncation":{truncation}}}"#)).unwrap();
        path
    }

    /// A database as left by an earlier Nellie: legacy tables with vectors.
    fn legacy_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            create_vector_tables(conn, &LEGACY_TABLES)?;
            let blob: Vec<u8> = vec![0.1f32; EMBEDDING_DIM]
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            conn.execute(
                "INSERT INTO lesson_embeddings (id, embedding) VALUES ('l1', ?)",
                [blob],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        db
    }

    #[test]
    fn fresh_install_records_current_spec() {
        let db = Database::open_in_memory().unwrap();
        init_storage(&db).unwrap();
        db.with_conn(|conn| {
            let meta = active_meta(conn)?.unwrap();
            assert_eq!(meta.spec, EmbeddingSpec::current());
            assert_eq!(meta.tables, CURRENT_TABLES);
            assert_eq!(meta.source, "fresh");
            for name in CURRENT_TABLES.all() {
                assert!(table_exists(conn, name)?);
            }
            for name in LEGACY_TABLES.all() {
                assert!(!table_exists(conn, name)?);
            }
            // Fresh install needs no tokenizer and passes the guard.
            let r = check_spec(conn, Path::new("/nonexistent/tokenizer.json"))?;
            assert!(r.is_ok());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn legacy_install_is_not_bootstrapped_without_tokenizer() {
        let db = legacy_db();
        init_storage(&db).unwrap();
        db.with_conn(|conn| {
            assert!(active_meta(conn)?.is_none());
            assert!(active_tables(conn).is_err());
            assert!(check_spec(conn, Path::new("/nonexistent/tokenizer.json")).is_err());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn legacy_install_with_128_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let tok = tokenizer_json(
            dir.path(),
            r#"{"max_length":128,"strategy":"LongestFirst","stride":0}"#,
        );
        let db = legacy_db();
        init_storage(&db).unwrap();
        db.with_conn(|conn| {
            let r = check_spec(conn, &tok)?;
            let msg = r.unwrap_err();
            assert!(msg.contains("max_len=128"), "{msg}");
            assert!(msg.contains("nellie reembed"), "{msg}");
            let meta = active_meta(conn)?.unwrap();
            assert_eq!(meta.spec.max_len, 128);
            assert_eq!(meta.spec.special_tokens, "intact");
            assert_eq!(meta.tables, LEGACY_TABLES);
            assert_eq!(meta.source, "pre-guard");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn legacy_install_without_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let tok = tokenizer_json(dir.path(), "null");
        let db = legacy_db();
        db.with_conn(|conn| {
            assert!(check_spec(conn, &tok)?.is_err());
            let meta = active_meta(conn)?.unwrap();
            assert_eq!(meta.spec.max_len, 256);
            assert_eq!(meta.spec.special_tokens, "truncated");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn legacy_install_with_matching_settings_passes() {
        let dir = tempfile::tempdir().unwrap();
        let tok = tokenizer_json(
            dir.path(),
            r#"{"max_length":256,"strategy":"LongestFirst","stride":0}"#,
        );
        let db = legacy_db();
        db.with_conn(|conn| {
            assert!(check_spec(conn, &tok)?.is_ok());
            assert_eq!(active_tables(conn)?, LEGACY_TABLES);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn bootstrap_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let tok128 = tokenizer_json(
            dir.path(),
            r#"{"max_length":128,"strategy":"LongestFirst","stride":0}"#,
        );
        let db = legacy_db();
        db.with_conn(|conn| {
            bootstrap_meta(conn, &tok128)?;
            // A later, different tokenizer.json must not rewrite history.
            let tok_none = tokenizer_json(dir.path(), "null");
            bootstrap_meta(conn, &tok_none)?;
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM embedding_meta", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1);
            assert_eq!(active_meta(conn)?.unwrap().spec.max_len, 128);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn guard_passes_after_switch_to_current() {
        let dir = tempfile::tempdir().unwrap();
        let tok = tokenizer_json(
            dir.path(),
            r#"{"max_length":128,"strategy":"LongestFirst","stride":0}"#,
        );
        let db = legacy_db();
        db.with_conn(|conn| {
            assert!(check_spec(conn, &tok)?.is_err());
            record_active(conn, &EmbeddingSpec::current(), &CURRENT_TABLES, "reembed")?;
            assert!(check_spec(conn, &tok)?.is_ok());
            assert_eq!(active_tables(conn)?, CURRENT_TABLES);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn unknown_table_name_is_rejected() {
        let db = Database::open_in_memory().unwrap();
        init_storage(&db).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE embedding_meta SET lesson_table = 'lessons; DROP TABLE lessons' WHERE active = 1",
                [],
            )
            .unwrap();
            assert!(active_tables(conn).is_err());
            Ok(())
        })
        .unwrap();
    }
}
