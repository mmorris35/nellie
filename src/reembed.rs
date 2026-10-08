//! `nellie reembed`: rebuild the vector index under the current embedding spec.
//!
//! Every lesson, checkpoint and code chunk is embedded again from the text
//! stored in the database, using the same text format as the insert paths,
//! into vector tables named for the current spec. The run is resumable: rows
//! already present in the new tables are skipped, and each batch is committed
//! on its own. When every row is present, one transaction makes the new
//! tables active in `embedding_meta`. Old tables are kept unless
//! `--drop-old` is given.

use std::fs::{File, OpenOptions, TryLockError};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::types::Value;
use rusqlite::{Connection, Row};

use crate::embeddings::{
    checkpoint_embedding_text, chunk_embedding_text, lesson_embedding_text, EmbeddingService,
    EmbeddingSpec,
};
use crate::error::StorageError;
use crate::storage::embedding_meta::{
    active_meta, create_vector_tables, record_active, table_exists, VectorTables, CURRENT_TABLES,
    KNOWN_TABLE_SETS,
};
use crate::storage::Database;
use crate::{Error, Result};

/// Anything that can turn texts into embedding vectors.
pub trait Embedder: Sync {
    /// Embed a batch of texts, returning one vector per text.
    fn embed(&self, texts: Vec<String>) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send;
}

impl Embedder for EmbeddingService {
    fn embed(&self, texts: Vec<String>) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send {
        self.embed_batch(texts)
    }
}

/// The three kinds of embedded rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Lessons (`title`, `content`).
    Lessons,
    /// Checkpoints (`working_on`).
    Checkpoints,
    /// Code chunks (`content`).
    Chunks,
}

impl Kind {
    /// All kinds, in processing order.
    pub const ALL: [Self; 3] = [Self::Lessons, Self::Checkpoints, Self::Chunks];

    /// Human-readable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lessons => "lessons",
            Self::Checkpoints => "checkpoints",
            Self::Chunks => "chunks",
        }
    }

    const fn source_table(self) -> &'static str {
        match self {
            Self::Lessons => "lessons",
            Self::Checkpoints => "checkpoints",
            Self::Chunks => "chunks",
        }
    }

    const fn text_columns(self) -> &'static str {
        match self {
            Self::Lessons => "title, content",
            Self::Checkpoints => "working_on",
            Self::Chunks => "content",
        }
    }

    const fn vector_table(self, tables: &VectorTables) -> &'static str {
        match self {
            Self::Lessons => tables.lessons,
            Self::Checkpoints => tables.checkpoints,
            Self::Chunks => tables.chunks,
        }
    }

    /// Build the embedded text from a row of `SELECT id, <text_columns>`,
    /// using the same function as the insert paths.
    fn text_from_row(self, row: &Row<'_>) -> rusqlite::Result<String> {
        Ok(match self {
            Self::Lessons => {
                lesson_embedding_text(&row.get::<_, String>(1)?, &row.get::<_, String>(2)?)
            }
            Self::Checkpoints => checkpoint_embedding_text(&row.get::<_, String>(1)?),
            Self::Chunks => chunk_embedding_text(&row.get::<_, String>(1)?),
        })
    }
}

/// Options for [`run_reembed`].
#[derive(Debug, Clone)]
pub struct ReembedOptions {
    /// Texts per embedding request.
    pub batch_size: usize,
    /// Embedding requests in flight at once (match the worker count).
    pub concurrency: usize,
    /// Drop vector tables that are no longer active once the switch is done.
    pub drop_old: bool,
}

impl Default for ReembedOptions {
    fn default() -> Self {
        Self {
            batch_size: 16,
            concurrency: 4,
            drop_old: false,
        }
    }
}

/// Progress of one kind, reported after every committed batch.
#[derive(Debug, Clone)]
pub struct Progress {
    /// Which rows.
    pub kind: Kind,
    /// Rows present in the new table so far.
    pub done: u64,
    /// Rows in the source table.
    pub total: u64,
    /// Rows embedded in this run.
    pub embedded: u64,
    /// Seconds since this kind started.
    pub elapsed_secs: f64,
}

/// Per-kind outcome.
#[derive(Debug, Clone, Default)]
pub struct KindReport {
    /// Rows in the source table.
    pub total: u64,
    /// Rows already present in the new table (from an earlier run).
    pub skipped: u64,
    /// Rows embedded in this run.
    pub embedded: u64,
    /// Rows whose stored text is empty (embedded anyway, but worth knowing).
    pub empty_text: u64,
    /// Vectors in the previously active table with no stored row to rebuild
    /// them from (left over from deleted items). They are not carried over.
    pub orphaned_old_vectors: u64,
    /// Rows that could not be embedded: `(id, error)`. While any remain the
    /// index is not switched.
    pub failed: Vec<(String, String)>,
    /// Seconds spent embedding this kind.
    pub elapsed_secs: f64,
}

/// Outcome of a reembed run.
#[derive(Debug, Clone)]
pub struct ReembedReport {
    /// Spec the index was built with before this run.
    pub previous_spec: EmbeddingSpec,
    /// Tables that were active before this run.
    pub previous_tables: VectorTables,
    /// Tables active after this run.
    pub active_tables: VectorTables,
    /// Whether this run switched the active tables.
    pub switched: bool,
    /// Tables dropped by `--drop-old`.
    pub dropped: Vec<&'static str>,
    /// Lessons.
    pub lessons: KindReport,
    /// Checkpoints.
    pub checkpoints: KindReport,
    /// Chunks.
    pub chunks: KindReport,
}

fn db_err(context: &str, e: &rusqlite::Error) -> Error {
    StorageError::Database(format!("{context}: {e}")).into()
}

fn count(conn: &Connection, sql: &str) -> Result<u64> {
    let n: i64 = conn
        .query_row(sql, [], |r| r.get(0))
        .map_err(|e| db_err("count failed", &e))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Source rows not yet present in `target`.
fn missing_count(conn: &Connection, kind: Kind, target: &str) -> Result<u64> {
    let src = kind.source_table();
    count(
        conn,
        &format!("SELECT COUNT(*) FROM {src} WHERE id NOT IN (SELECT id FROM {target})"),
    )
}

/// Next page of source rows after `after` (keyset pagination by id).
fn next_page(
    conn: &Connection,
    kind: Kind,
    after: &Value,
    limit: usize,
) -> Result<Vec<(Value, String)>> {
    let sql = format!(
        "SELECT id, {} FROM {} WHERE id > ? ORDER BY id LIMIT ?",
        kind.text_columns(),
        kind.source_table()
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| db_err("failed to prepare page query", &e))?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = stmt
        .query_map(rusqlite::params![after, limit], |row| {
            Ok((row.get::<_, Value>(0)?, kind.text_from_row(row)?))
        })
        .map_err(|e| db_err("failed to read source rows", &e))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_err("failed to read source row", &e))
}

fn has_vector(conn: &Connection, table: &str, id: &Value) -> Result<bool> {
    conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?)"),
        [id],
        |r| r.get(0),
    )
    .map_err(|e| db_err("failed to check vector", &e))
}

fn insert_vectors(conn: &Connection, table: &str, rows: &[(Value, Vec<f32>)]) -> Result<()> {
    let sql = format!("INSERT INTO {table} (id, embedding) VALUES (?, ?)");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| db_err("failed to prepare insert", &e))?;
    for (id, embedding) in rows {
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        stmt.execute(rusqlite::params![id, blob])
            .map_err(|e| db_err("failed to insert vector", &e))?;
    }
    Ok(())
}

fn id_string(id: &Value) -> String {
    match id {
        Value::Integer(n) => n.to_string(),
        Value::Text(t) => t.clone(),
        other => format!("{other:?}"),
    }
}

/// A batch failed: embed its rows one at a time to find the rows that cannot
/// be embedded. If none succeed the failure is not row-specific (e.g. the
/// model is unavailable) and `batch_err` is returned.
async fn embed_one_by_one<E: Embedder>(
    embedder: &E,
    chunk: &[(Value, String)],
    batch_err: Error,
    rows: &mut Vec<(Value, Vec<f32>)>,
    failed: &mut Vec<(String, String)>,
) -> Result<()> {
    let mut chunk_failed = Vec::new();
    let before = rows.len();
    for (id, text) in chunk {
        match embedder.embed(vec![text.clone()]).await {
            Ok(mut v) if v.len() == 1 => rows.push((id.clone(), v.remove(0))),
            Ok(v) => chunk_failed.push((id_string(id), format!("{} vectors for 1 text", v.len()))),
            Err(e) => chunk_failed.push((id_string(id), e.to_string())),
        }
    }
    if rows.len() == before {
        return Err(batch_err);
    }
    for (id, e) in &chunk_failed {
        tracing::warn!(id = %id, error = %e, "Row could not be embedded");
    }
    failed.extend(chunk_failed);
    Ok(())
}

async fn reembed_kind<E: Embedder>(
    db: &Database,
    embedder: &E,
    kind: Kind,
    target: &str,
    previous: &str,
    opts: &ReembedOptions,
    progress: &mut (dyn FnMut(&Progress) + Send),
) -> Result<KindReport> {
    let src = kind.source_table();
    let start = Instant::now();
    let mut report = db.with_conn(|conn| {
        let total = count(conn, &format!("SELECT COUNT(*) FROM {src}"))?;
        let missing = missing_count(conn, kind, target)?;
        let empty_text = count(
            conn,
            &format!(
                "SELECT COUNT(*) FROM {src} WHERE length(trim({})) = 0",
                kind.text_columns().replace(", ", " || ")
            ),
        )?;
        let orphaned_old_vectors = if previous != target && table_exists(conn, previous)? {
            count(
                conn,
                &format!("SELECT COUNT(*) FROM {previous} WHERE id NOT IN (SELECT id FROM {src})"),
            )?
        } else {
            0
        };
        Ok(KindReport {
            total,
            skipped: total - missing.min(total),
            empty_text,
            orphaned_old_vectors,
            ..KindReport::default()
        })
    })?;

    let batch_size = opts.batch_size.max(1);
    let group = batch_size * opts.concurrency.max(1);
    // Integers sort before text in SQLite, so this precedes every id.
    let mut after = Value::Integer(i64::MIN);

    loop {
        // Collect up to `group` rows that are not in the new table yet.
        let mut pending: Vec<(Value, String)> = Vec::with_capacity(group);
        let mut exhausted = false;
        while pending.len() < group {
            let page = db.with_conn(|conn| next_page(conn, kind, &after, group))?;
            if page.is_empty() {
                exhausted = true;
                break;
            }
            after = page[page.len() - 1].0.clone();
            for (id, text) in page {
                if !db.with_conn(|conn| has_vector(conn, target, &id))? {
                    pending.push((id, text));
                }
            }
        }
        if pending.is_empty() {
            break;
        }

        // Embed in parallel requests, then commit the whole group at once.
        let requests = pending
            .chunks(batch_size)
            .map(|chunk| embedder.embed(chunk.iter().map(|(_, t)| t.clone()).collect()));
        let results = futures::future::join_all(requests).await;
        let mut rows = Vec::with_capacity(pending.len());
        for (chunk, result) in pending.chunks(batch_size).zip(results) {
            match result {
                Ok(embeddings) => {
                    if embeddings.len() != chunk.len() {
                        return Err(Error::internal(format!(
                            "embedder returned {} vectors for {} texts",
                            embeddings.len(),
                            chunk.len()
                        )));
                    }
                    rows.extend(chunk.iter().map(|(id, _)| id.clone()).zip(embeddings));
                }
                Err(e) => {
                    embed_one_by_one(embedder, chunk, e, &mut rows, &mut report.failed).await?;
                }
            }
        }
        db.with_transaction(|conn| insert_vectors(conn, target, &rows))?;

        report.embedded += rows.len() as u64;
        progress(&Progress {
            kind,
            done: report.skipped + report.embedded,
            total: report.total,
            embedded: report.embedded,
            elapsed_secs: start.elapsed().as_secs_f64(),
        });

        if exhausted {
            break;
        }
    }

    report.elapsed_secs = start.elapsed().as_secs_f64();
    Ok(report)
}

/// Rebuild the vector index under [`EmbeddingSpec::current`].
///
/// The caller must have recorded the existing index's spec first (see
/// `embedding_meta::bootstrap_meta`) and must hold the exclusive
/// [`DbLock`] so no server writes meanwhile.
///
/// # Errors
///
/// Returns an error if embedding or storage fails; batches committed before
/// the failure are kept, and a rerun continues from there.
pub async fn run_reembed<E: Embedder>(
    db: &Database,
    embedder: &E,
    opts: &ReembedOptions,
    progress: &mut (dyn FnMut(&Progress) + Send),
) -> Result<ReembedReport> {
    let current = EmbeddingSpec::current();
    let before = db.with_conn(active_meta)?.ok_or_else(|| {
        StorageError::Vector("embedding_meta has no active row; cannot reembed".to_string())
    })?;

    // If the active tables already match the current spec, top them up in
    // place; otherwise build the tables named for the current spec.
    let target = if before.spec == current {
        before.tables
    } else {
        CURRENT_TABLES
    };
    db.with_conn(|conn| create_vector_tables(conn, &target))?;

    let mut reports = Vec::with_capacity(3);
    for kind in Kind::ALL {
        reports.push(
            reembed_kind(
                db,
                embedder,
                kind,
                kind.vector_table(&target),
                kind.vector_table(&before.tables),
                opts,
                progress,
            )
            .await?,
        );
    }

    let failed: Vec<String> = Kind::ALL
        .iter()
        .zip(&reports)
        .flat_map(|(kind, r)| {
            r.failed
                .iter()
                .map(move |(id, e)| format!("{} {id}: {e}", kind.name()))
        })
        .collect();
    if !failed.is_empty() {
        return Err(Error::internal(format!(
            "{} rows could not be embedded, so the index was not switched; \
             fix or delete them and run `nellie reembed` again:\n  {}",
            failed.len(),
            failed.join("\n  ")
        )));
    }

    // One transaction: verify completeness, then switch.
    let switched = db.with_transaction(|conn| {
        for kind in Kind::ALL {
            let missing = missing_count(conn, kind, kind.vector_table(&target))?;
            if missing > 0 {
                return Err(Error::internal(format!(
                    "{missing} {} were added while reembedding; run `nellie reembed` again",
                    kind.name()
                )));
            }
        }
        let active = active_meta(conn)?;
        if active.is_some_and(|m| m.spec == current && m.tables == target) {
            return Ok(false);
        }
        record_active(conn, &current, &target, "reembed")?;
        Ok(true)
    })?;

    let dropped = if opts.drop_old {
        drop_inactive_tables(db, &target)?
    } else {
        Vec::new()
    };

    let mut reports = reports.into_iter();
    Ok(ReembedReport {
        previous_spec: before.spec,
        previous_tables: before.tables,
        active_tables: target,
        switched,
        dropped,
        lessons: reports.next().unwrap_or_default(),
        checkpoints: reports.next().unwrap_or_default(),
        chunks: reports.next().unwrap_or_default(),
    })
}

/// Drop known vector tables other than `active`. Only valid once the active
/// tables match the current spec.
fn drop_inactive_tables(db: &Database, active: &VectorTables) -> Result<Vec<&'static str>> {
    db.with_transaction(|conn| {
        let meta = active_meta(conn)?;
        if !meta.is_some_and(|m| m.spec == EmbeddingSpec::current() && m.tables == *active) {
            return Err(Error::internal(
                "refusing to drop old vector tables: the index has not been switched to the \
                 current embedding spec",
            ));
        }
        let mut dropped = Vec::new();
        for set in &KNOWN_TABLE_SETS {
            for name in set.all() {
                if active.all().contains(&name) || !table_exists(conn, name)? {
                    continue;
                }
                conn.execute_batch(&format!("DROP TABLE {name}"))
                    .map_err(|e| db_err("failed to drop table", &e))?;
                dropped.push(name);
            }
        }
        Ok(dropped)
    })
}

/// Advisory lock that keeps `nellie reembed` and servers off the same
/// database at the same time.
///
/// Servers hold a shared lock on `<database>.lock` for their lifetime;
/// `nellie reembed` needs the exclusive lock. The OS releases the lock when
/// the process exits, so a crash never leaves a stale lock behind.
#[derive(Debug)]
pub struct DbLock {
    _file: File,
    path: PathBuf,
}

impl DbLock {
    /// Lock file path for a database.
    #[must_use]
    pub fn path_for(db_path: &Path) -> PathBuf {
        let mut name = db_path.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }

    fn open(db_path: &Path) -> Result<(File, PathBuf)> {
        let path = Self::path_for(db_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        Ok((file, path))
    }

    /// Take a shared lock (servers). Several servers may share it.
    ///
    /// # Errors
    ///
    /// Returns an error if `nellie reembed` holds the lock or locking fails.
    // `File::try_lock*` is std since Rust 1.89. The declared rust-version is
    // older, but the `ort` dependency already needs a recent toolchain.
    #[allow(clippy::incompatible_msrv)]
    pub fn shared(db_path: &Path) -> Result<Self> {
        let (file, path) = Self::open(db_path)?;
        match file.try_lock_shared() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(Error::internal(format!(
                "`nellie reembed` is running against {}; wait for it to finish",
                db_path.display()
            ))),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// Take the exclusive lock (`nellie reembed`).
    ///
    /// # Errors
    ///
    /// Returns an error if a server holds the lock or locking fails.
    #[allow(clippy::incompatible_msrv)]
    pub fn exclusive(db_path: &Path) -> Result<Self> {
        let (file, path) = Self::open(db_path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(Error::internal(format!(
                "a Nellie server is running against {}; stop it before running `nellie reembed`",
                db_path.display()
            ))),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// Path of the lock file.
    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::placeholder_embedding;
    use crate::storage::embedding_meta::{active_tables, bootstrap_meta, LEGACY_TABLES};
    use crate::storage::{
        init_storage, insert_checkpoint, insert_chunk, insert_lesson, migrate,
        search_lessons_by_embedding, CheckpointRecord, ChunkRecord, LessonRecord,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Deterministic embedder; fails once `fail_after` requests have run.
    struct FakeEmbedder {
        calls: AtomicUsize,
        fail_after: Option<usize>,
        poison: Option<&'static str>,
    }

    impl FakeEmbedder {
        fn ok() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail_after: None,
                poison: None,
            }
        }
        fn failing_after(n: usize) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail_after: Some(n),
                poison: None,
            }
        }
        /// Fails any request containing a text with `marker`.
        fn poisoned(marker: &'static str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail_after: None,
                poison: Some(marker),
            }
        }
    }

    impl Embedder for FakeEmbedder {
        fn embed(&self, texts: Vec<String>) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let fail = self.fail_after.is_some_and(|limit| n >= limit)
                || self
                    .poison
                    .is_some_and(|m| texts.iter().any(|t| t.contains(m)));
            async move {
                if fail {
                    return Err(Error::internal("simulated crash"));
                }
                Ok(texts.iter().map(|t| placeholder_embedding(t)).collect())
            }
        }
    }

    /// An install from before the embedding guard, with 128-token vectors
    /// in the legacy tables.
    fn legacy_db(dir: &Path, lessons: usize) -> Database {
        let db = Database::open(dir.join("nellie.db")).unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            create_vector_tables(conn, &LEGACY_TABLES)?;
            for i in 0..lessons {
                let lesson = LessonRecord::new(format!("Lesson {i}"), format!("Body {i}"), vec![]);
                insert_lesson(conn, &lesson)?;
                let blob: Vec<u8> = placeholder_embedding("old")
                    .iter()
                    .flat_map(|f| f.to_le_bytes())
                    .collect();
                conn.execute(
                    "INSERT INTO lesson_embeddings (id, embedding) VALUES (?, ?)",
                    rusqlite::params![lesson.id, blob],
                )
                .unwrap();
            }
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent", "working on reembed", serde_json::json!({})),
            )?;
            insert_chunk(
                conn,
                &ChunkRecord::new("/src/a.rs", 0, 1, 3, "fn a() {}", "hash"),
            )?;
            Ok(())
        })
        .unwrap();
        init_storage(&db).unwrap();
        let tok = dir.join("tokenizer.json");
        std::fs::write(&tok, r#"{"truncation":{"max_length":128}}"#).unwrap();
        db.with_conn(|conn| bootstrap_meta(conn, &tok)).unwrap();
        db
    }

    fn opts() -> ReembedOptions {
        ReembedOptions {
            batch_size: 2,
            concurrency: 2,
            drop_old: false,
        }
    }

    fn rows(db: &Database, table: &str) -> u64 {
        db.with_conn(|conn| count(conn, &format!("SELECT COUNT(*) FROM {table}")))
            .unwrap()
    }

    #[tokio::test]
    async fn reembed_is_resumable_and_switches_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 10);

        // First run "crashes" after two embedding requests.
        let err = run_reembed(&db, &FakeEmbedder::failing_after(2), &opts(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("simulated crash"));
        // Nothing switched; partial progress (one committed group) kept.
        assert_eq!(db.with_conn(active_tables).unwrap(), LEGACY_TABLES);
        assert_eq!(rows(&db, CURRENT_TABLES.lessons), 4);

        // Rerun completes without duplicates and switches.
        let report = run_reembed(&db, &FakeEmbedder::ok(), &opts(), &mut |_| {})
            .await
            .unwrap();
        assert!(report.switched);
        assert_eq!(report.lessons.skipped, 4);
        assert_eq!(report.lessons.embedded, 6);
        assert_eq!(report.checkpoints.embedded, 1);
        assert_eq!(report.chunks.embedded, 1);
        assert_eq!(rows(&db, CURRENT_TABLES.lessons), 10);
        assert_eq!(rows(&db, CURRENT_TABLES.checkpoints), 1);
        assert_eq!(rows(&db, CURRENT_TABLES.chunks), 1);
        // Old tables kept for rollback.
        assert_eq!(rows(&db, LEGACY_TABLES.lessons), 10);

        db.with_conn(|conn| {
            let meta = active_meta(conn)?.unwrap();
            assert_eq!(meta.spec, EmbeddingSpec::current());
            assert_eq!(meta.tables, CURRENT_TABLES);
            Ok(())
        })
        .unwrap();

        // A third run is a no-op top-up.
        let again = run_reembed(&db, &FakeEmbedder::ok(), &opts(), &mut |_| {})
            .await
            .unwrap();
        assert!(!again.switched);
        assert_eq!(again.lessons.embedded, 0);
    }

    #[tokio::test]
    async fn search_reads_active_table_after_switch() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 3);
        let target = db
            .with_conn(|conn| crate::storage::list_lessons(conn))
            .unwrap()
            .into_iter()
            .find(|l| l.title == "Lesson 1")
            .unwrap();
        let query = placeholder_embedding(&lesson_embedding_text(&target.title, &target.content));

        // Before the switch, the legacy vectors (all identical) are searched.
        let before = db
            .with_conn(|conn| search_lessons_by_embedding(conn, &query, 1))
            .unwrap();
        assert!(before[0].distance > 0.01);

        run_reembed(&db, &FakeEmbedder::ok(), &opts(), &mut |_| {})
            .await
            .unwrap();

        let after = db
            .with_conn(|conn| search_lessons_by_embedding(conn, &query, 1))
            .unwrap();
        assert_eq!(after[0].record.id, target.id);
        assert!(after[0].distance < 1e-3);
    }

    #[tokio::test]
    async fn drop_old_removes_legacy_tables_after_switch() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 2);

        // Not switched yet: dropping is refused.
        assert!(drop_inactive_tables(&db, &CURRENT_TABLES).is_err());

        let mut o = opts();
        o.drop_old = true;
        let report = run_reembed(&db, &FakeEmbedder::ok(), &o, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(report.dropped.len(), 3);
        db.with_conn(|conn| {
            for name in LEGACY_TABLES.all() {
                assert!(!table_exists(conn, name)?);
            }
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn orphaned_vectors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 2);
        db.with_conn(|conn| {
            let blob: Vec<u8> = placeholder_embedding("gone")
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            conn.execute(
                "INSERT INTO lesson_embeddings (id, embedding) VALUES ('deleted-lesson', ?)",
                [blob],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let report = run_reembed(&db, &FakeEmbedder::ok(), &opts(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(report.lessons.orphaned_old_vectors, 1);
        assert_eq!(report.lessons.embedded, 2);
    }

    #[tokio::test]
    async fn unembeddable_rows_are_reported_and_block_the_switch() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 5);
        let bad = LessonRecord::new("POISON", "cannot embed", vec![]);
        db.with_conn(|conn| insert_lesson(conn, &bad)).unwrap();

        let err = run_reembed(&db, &FakeEmbedder::poisoned("POISON"), &opts(), &mut |_| {})
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("1 rows could not be embedded"), "{err}");
        assert!(err.contains(&format!("lessons {}", bad.id)), "{err}");
        // Every other row was embedded; the index was not switched.
        assert_eq!(rows(&db, CURRENT_TABLES.lessons), 5);
        assert_eq!(db.with_conn(active_tables).unwrap(), LEGACY_TABLES);

        // Once the row is removed, a rerun switches.
        db.with_conn(|conn| crate::storage::delete_lesson(conn, &bad.id))
            .unwrap();
        let report = run_reembed(&db, &FakeEmbedder::poisoned("POISON"), &opts(), &mut |_| {})
            .await
            .unwrap();
        assert!(report.switched);
    }

    #[test]
    fn lock_excludes_reembed_while_server_runs() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nellie.db");
        let server = DbLock::shared(&db_path).unwrap();
        // A second server may share the database.
        let second = DbLock::shared(&db_path).unwrap();
        let err = DbLock::exclusive(&db_path).unwrap_err();
        assert!(err.to_string().contains("server is running"));
        drop(server);
        drop(second);
        let reembed = DbLock::exclusive(&db_path).unwrap();
        assert!(DbLock::shared(&db_path)
            .unwrap_err()
            .to_string()
            .contains("reembed"));
        drop(reembed);
    }
}
