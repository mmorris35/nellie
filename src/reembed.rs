//! `nellie reembed`: rebuild the vector index under the current embedding spec.
//!
//! Every lesson, checkpoint and code chunk is embedded again from the text
//! stored in the database, using the same text format as the insert paths,
//! into vector tables named for the current spec. The run is resumable: rows
//! already present in the new tables are skipped, and each batch is committed
//! on its own. When every row is present, one transaction makes the new
//! tables active in `embedding_meta`. Old tables are kept unless
//! `--drop-old` is given.
//!
//! With `--no-switch` the new tables are built while an older Nellie keeps
//! serving the database: every write is a short transaction, and the active
//! tables are left alone. The time the build started is kept in
//! `reembed_state`. The switching run (Nellie stopped) then catches up with
//! what changed meanwhile: it removes vectors whose row was deleted,
//! re-embeds lessons edited since the build started, embeds rows added since,
//! and switches only when the new tables match the stored rows exactly.
//! Checkpoints are never edited, and changed code files get new chunk rows
//! with fresh ids, so for those adding and removing rows is enough.

use std::fs::{File, OpenOptions, TryLockError};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::types::Value;
use rusqlite::{Connection, Row};

use crate::embeddings::{
    checkpoint_embedding_text, chunk_embedding_text, lesson_embedding_text, lesson_section_texts,
    EmbeddingService, EmbeddingSpec,
};
use crate::error::StorageError;
use crate::storage::embedding_meta::{
    active_meta, create_vector_tables, is_section_table, owner_column, record_active, table_exists,
    VectorTables, CURRENT_TABLES, KNOWN_TABLE_SETS,
};
use crate::storage::Database;
use crate::storage::{insert_lesson_vectors, section_id};
use crate::{Error, Result};

/// Anything that can turn texts into embedding vectors.
pub trait Embedder: Sync {
    /// Embed a batch of texts, returning one vector per text.
    fn embed(&self, texts: Vec<String>) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send;

    /// Texts to embed for a lesson stored with one vector per section.
    ///
    /// The default measures length in whitespace-separated words, which is
    /// only good enough for tests; [`EmbeddingService`] uses its tokenizer.
    ///
    /// # Errors
    ///
    /// Returns an error if the text cannot be measured.
    fn lesson_texts(&self, title: &str, content: &str) -> Result<Vec<String>> {
        Ok(lesson_section_texts(title, content, |t| {
            t.split_whitespace().count()
        }))
    }
}

impl Embedder for EmbeddingService {
    fn embed(&self, texts: Vec<String>) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send {
        self.embed_batch(texts)
    }

    fn lesson_texts(&self, title: &str, content: &str) -> Result<Vec<String>> {
        Self::lesson_texts(self, title, content)
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

    /// The text columns of a row of `SELECT id, <text_columns>`.
    fn columns_from_row(self, row: &Row<'_>) -> rusqlite::Result<Vec<String>> {
        let n = match self {
            Self::Lessons => 2,
            Self::Checkpoints | Self::Chunks => 1,
        };
        (1..=n).map(|i| row.get::<_, String>(i)).collect()
    }

    /// The texts to embed for a row (one per vector), using the same
    /// functions as the insert paths. Lessons stored in a section table get
    /// one text per section.
    fn texts<E: Embedder>(
        self,
        embedder: &E,
        table: &str,
        columns: &[String],
    ) -> Result<Vec<String>> {
        let col = |i: usize| columns.get(i).map_or("", String::as_str);
        Ok(match self {
            Self::Lessons if is_section_table(table) => embedder.lesson_texts(col(0), col(1))?,
            Self::Lessons => vec![lesson_embedding_text(col(0), col(1))],
            Self::Checkpoints => vec![checkpoint_embedding_text(col(0))],
            Self::Chunks => vec![chunk_embedding_text(col(0))],
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
    /// Make the new tables active once they are complete. Without it the
    /// new tables are only built, which is safe while a server is running.
    pub switch: bool,
}

impl Default for ReembedOptions {
    fn default() -> Self {
        Self {
            batch_size: 16,
            concurrency: 4,
            drop_old: false,
            switch: true,
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
    /// Vectors removed from the new table because their row was deleted
    /// after they were embedded.
    pub orphans_removed: u64,
    /// Lessons re-embedded because they were edited after the build started.
    pub refreshed: u64,
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
    /// When the build of the new tables started (Unix seconds), if they are
    /// not active yet.
    pub build_started_at: Option<i64>,
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
    let owner = owner_column(target);
    count(
        conn,
        &format!("SELECT COUNT(*) FROM {src} WHERE id NOT IN (SELECT {owner} FROM {target})"),
    )
}

/// Next page of source rows after `after` (keyset pagination by id), with
/// their text columns.
fn next_page(
    conn: &Connection,
    kind: Kind,
    after: &Value,
    limit: usize,
) -> Result<Vec<(Value, Vec<String>)>> {
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
            Ok((row.get::<_, Value>(0)?, kind.columns_from_row(row)?))
        })
        .map_err(|e| db_err("failed to read source rows", &e))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_err("failed to read source row", &e))
}

/// Does the row `id` have its vectors in `table`? A lesson's sections are
/// always written together, so section 0 stands for all of them (and keeps
/// this a primary-key lookup).
fn has_vector(conn: &Connection, table: &str, id: &Value) -> Result<bool> {
    let key = match id {
        Value::Text(lesson) if is_section_table(table) => Value::Text(section_id(lesson, 0)),
        other => other.clone(),
    };
    conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?)"),
        [key],
        |r| r.get(0),
    )
    .map_err(|e| db_err("failed to check vector", &e))
}

/// Insert vectors (one or more per row), skipping rows that already have
/// them or whose row has been deleted meanwhile (a server may write the same
/// tables). Returns how many rows got vectors. Run inside a transaction.
fn insert_vectors(
    conn: &Connection,
    kind: Kind,
    table: &str,
    rows: &[(Value, Vec<Vec<f32>>)],
) -> Result<u64> {
    let sql = format!("INSERT INTO {table} (id, embedding) VALUES (?, ?)");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| db_err("failed to prepare insert", &e))?;
    let mut inserted = 0;
    for (id, embeddings) in rows {
        if has_vector(conn, table, id)? || !has_source(conn, kind, id)? {
            continue;
        }
        if let (Kind::Lessons, Value::Text(lesson)) = (kind, id) {
            insert_lesson_vectors(conn, table, lesson, embeddings)?;
        } else {
            let [embedding] = embeddings.as_slice() else {
                return Err(Error::internal(format!(
                    "{} rows take one vector, got {}",
                    kind.name(),
                    embeddings.len()
                )));
            };
            let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
            stmt.execute(rusqlite::params![id, blob])
                .map_err(|e| db_err("failed to insert vector", &e))?;
        }
        inserted += 1;
    }
    Ok(inserted)
}

fn has_source(conn: &Connection, kind: Kind, id: &Value) -> Result<bool> {
    conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE id = ?)",
            kind.source_table()
        ),
        [id],
        |r| r.get(0),
    )
    .map_err(|e| db_err("failed to check source row", &e))
}

/// Vectors in `table` whose source row no longer exists.
fn orphan_count(conn: &Connection, kind: Kind, table: &str) -> Result<u64> {
    let src = kind.source_table();
    let owner = owner_column(table);
    count(
        conn,
        &format!("SELECT COUNT(*) FROM {table} WHERE {owner} NOT IN (SELECT id FROM {src})"),
    )
}

/// Delete the vectors with these vector-row ids from `table`.
fn delete_vectors(conn: &Connection, table: &str, ids: &[Value]) -> Result<u64> {
    let mut stmt = conn
        .prepare(&format!("DELETE FROM {table} WHERE id = ?"))
        .map_err(|e| db_err("failed to prepare delete", &e))?;
    let mut deleted = 0;
    for id in ids {
        deleted += stmt
            .execute([id])
            .map_err(|e| db_err("failed to delete vector", &e))?;
    }
    Ok(deleted as u64)
}

fn select_ids(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Value>> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| db_err("failed to prepare id query", &e))?;
    let ids = stmt
        .query_map(params, |r| r.get::<_, Value>(0))
        .map_err(|e| db_err("failed to read ids", &e))?;
    ids.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_err("failed to read id", &e))
}

/// Remove vectors in `table` whose source row has been deleted. Returns how
/// many vectors were removed.
fn remove_orphans(db: &Database, kind: Kind, table: &str) -> Result<u64> {
    let src = kind.source_table();
    let owner = owner_column(table);
    db.with_transaction(|conn| {
        let ids = select_ids(
            conn,
            &format!("SELECT id FROM {table} WHERE {owner} NOT IN (SELECT id FROM {src})"),
            [],
        )?;
        delete_vectors(conn, table, &ids)
    })
}

/// Remove the vectors of lessons edited at or after `since`, so they are
/// embedded again from their current text. Returns how many lessons lost
/// their vectors.
fn remove_edited_lessons(db: &Database, table: &str, since: i64) -> Result<u64> {
    let owner = owner_column(table);
    db.with_transaction(|conn| {
        let edited = format!(
            "SELECT id FROM lessons WHERE updated_at >= ? AND id IN (SELECT {owner} FROM {table})"
        );
        let lessons = select_ids(conn, &edited, [since])?;
        let ids = select_ids(
            conn,
            &format!("SELECT id FROM {table} WHERE {owner} IN ({edited})"),
            [since],
        )?;
        delete_vectors(conn, table, &ids)?;
        Ok(u64::try_from(lessons.len()).unwrap_or(u64::MAX))
    })
}

/// Record when building `target` started, keeping the earliest time if an
/// earlier run already started it. Returns the recorded time.
fn record_build_start(conn: &Connection, target: &VectorTables) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO reembed_state (lesson_table, started_at) VALUES (?, ?)",
        rusqlite::params![target.lessons, chrono::Utc::now().timestamp()],
    )
    .map_err(|e| db_err("failed to record reembed start", &e))?;
    conn.query_row(
        "SELECT started_at FROM reembed_state WHERE lesson_table = ?",
        [target.lessons],
        |r| r.get(0),
    )
    .map_err(|e| db_err("failed to read reembed start", &e))
}

fn id_string(id: &Value) -> String {
    match id {
        Value::Integer(n) => n.to_string(),
        Value::Text(t) => t.clone(),
        other => format!("{other:?}"),
    }
}

/// One text of a pending row: `(row index, text index, text)`.
type PendingText<'a> = (usize, usize, &'a str);

/// A batch failed: embed its texts one at a time to find the ones that
/// cannot be embedded. If none succeed the failure is not row-specific (e.g.
/// the model is unavailable) and `batch_err` is returned. Failures are
/// recorded per row in `errors`.
async fn embed_one_by_one<E: Embedder>(
    embedder: &E,
    chunk: &[PendingText<'_>],
    batch_err: Error,
    vectors: &mut [Vec<Option<Vec<f32>>>],
    errors: &mut [Option<String>],
) -> Result<()> {
    let mut any_ok = false;
    for &(row, index, text) in chunk {
        match embedder.embed(vec![text.to_string()]).await {
            Ok(mut v) if v.len() == 1 => {
                vectors[row][index] = Some(v.remove(0));
                any_ok = true;
            }
            Ok(v) => errors[row] = Some(format!("{} vectors for 1 text", v.len())),
            Err(e) => errors[row] = Some(e.to_string()),
        }
    }
    if any_ok {
        Ok(())
    } else {
        Err(batch_err)
    }
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
                &format!(
                    "SELECT COUNT(*) FROM {previous} WHERE {} NOT IN (SELECT id FROM {src})",
                    owner_column(previous)
                ),
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
        // Collect up to `group` rows that are not in the new table yet, with
        // the texts to embed for each (several for a sectioned lesson).
        let mut pending: Vec<(Value, Vec<String>)> = Vec::with_capacity(group);
        let mut exhausted = false;
        while pending.len() < group {
            let page = db.with_conn(|conn| next_page(conn, kind, &after, group))?;
            if page.is_empty() {
                exhausted = true;
                break;
            }
            after = page[page.len() - 1].0.clone();
            for (id, columns) in page {
                if db.with_conn(|conn| has_vector(conn, target, &id))? {
                    continue;
                }
                match kind.texts(embedder, target, &columns) {
                    Ok(texts) => pending.push((id, texts)),
                    Err(e) => {
                        tracing::warn!(id = %id_string(&id), error = %e, "Row could not be embedded");
                        report.failed.push((id_string(&id), e.to_string()));
                    }
                }
            }
        }
        if pending.is_empty() {
            if exhausted {
                break;
            }
            continue;
        }

        // Embed all texts in parallel requests, then commit the whole group
        // at once.
        let texts: Vec<PendingText<'_>> = pending
            .iter()
            .enumerate()
            .flat_map(|(row, (_, texts))| {
                texts
                    .iter()
                    .enumerate()
                    .map(move |(index, text)| (row, index, text.as_str()))
            })
            .collect();
        let requests = texts
            .chunks(batch_size)
            .map(|chunk| embedder.embed(chunk.iter().map(|&(_, _, t)| t.to_string()).collect()));
        let results = futures::future::join_all(requests).await;
        let mut vectors: Vec<Vec<Option<Vec<f32>>>> =
            pending.iter().map(|(_, t)| vec![None; t.len()]).collect();
        let mut errors: Vec<Option<String>> = vec![None; pending.len()];
        for (chunk, result) in texts.chunks(batch_size).zip(results) {
            match result {
                Ok(embeddings) => {
                    if embeddings.len() != chunk.len() {
                        return Err(Error::internal(format!(
                            "embedder returned {} vectors for {} texts",
                            embeddings.len(),
                            chunk.len()
                        )));
                    }
                    for (&(row, index, _), embedding) in chunk.iter().zip(embeddings) {
                        vectors[row][index] = Some(embedding);
                    }
                }
                Err(e) => {
                    embed_one_by_one(embedder, chunk, e, &mut vectors, &mut errors).await?;
                }
            }
        }
        let mut rows = Vec::with_capacity(pending.len());
        for (((id, _), row_vectors), error) in pending.into_iter().zip(vectors).zip(errors) {
            match (error, row_vectors.into_iter().collect::<Option<Vec<_>>>()) {
                (None, Some(embeddings)) => rows.push((id, embeddings)),
                (error, _) => {
                    let error = error.unwrap_or_else(|| "missing vector".to_string());
                    tracing::warn!(id = %id_string(&id), error = %error, "Row could not be embedded");
                    report.failed.push((id_string(&id), error));
                }
            }
        }
        report.embedded += db.with_transaction(|conn| insert_vectors(conn, kind, target, &rows))?;
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
/// [`DbLock`] so no server of this version writes meanwhile. With
/// `opts.switch` unset, an older server (which takes no lock) may keep
/// writing; the switching run catches up with its changes.
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
    let in_place = before.spec == current;
    let target = if in_place {
        before.tables
    } else {
        CURRENT_TABLES
    };
    let mut report = ReembedReport {
        previous_spec: before.spec.clone(),
        previous_tables: before.tables,
        active_tables: before.tables,
        switched: false,
        build_started_at: None,
        dropped: Vec::new(),
        lessons: KindReport::default(),
        checkpoints: KindReport::default(),
        chunks: KindReport::default(),
    };
    if in_place && !opts.switch {
        // The active tables are what a running server writes; nothing to
        // pre-build.
        return Ok(report);
    }

    db.with_conn(|conn| create_vector_tables(conn, &target))?;
    if !in_place {
        report.build_started_at =
            Some(db.with_transaction(|conn| record_build_start(conn, &target))?);
    }

    let mut reports = Vec::with_capacity(3);
    for kind in Kind::ALL {
        let table = kind.vector_table(&target);
        // Catch up with deletions and edits made while the tables were built.
        // Only the switching run needs this; it runs with Nellie stopped.
        let (orphans_removed, refreshed) = if opts.switch {
            let orphans = remove_orphans(db, kind, table)?;
            let edited = match (kind, report.build_started_at) {
                (Kind::Lessons, Some(since)) => remove_edited_lessons(db, table, since)?,
                _ => 0,
            };
            (orphans, edited)
        } else {
            (0, 0)
        };
        let mut r = reembed_kind(
            db,
            embedder,
            kind,
            table,
            kind.vector_table(&before.tables),
            opts,
            progress,
        )
        .await?;
        r.orphans_removed = orphans_removed;
        r.refreshed = refreshed;
        reports.push(r);
    }
    let mut kinds = reports.into_iter();
    report.lessons = kinds.next().unwrap_or_default();
    report.checkpoints = kinds.next().unwrap_or_default();
    report.chunks = kinds.next().unwrap_or_default();

    let failed: Vec<String> = Kind::ALL
        .iter()
        .zip([&report.lessons, &report.checkpoints, &report.chunks])
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
    if !opts.switch {
        return Ok(report);
    }

    // One transaction: verify the new tables match the stored rows exactly,
    // then switch.
    report.switched = db.with_transaction(|conn| {
        for kind in Kind::ALL {
            let table = kind.vector_table(&target);
            let missing = missing_count(conn, kind, table)?;
            let orphans = orphan_count(conn, kind, table)?;
            if missing > 0 || orphans > 0 {
                return Err(Error::internal(format!(
                    "{} changed while reembedding ({missing} added, {orphans} deleted); \
                     is a Nellie server still running? Stop it and run `nellie reembed` again",
                    kind.name()
                )));
            }
        }
        conn.execute(
            "DELETE FROM reembed_state WHERE lesson_table = ?",
            [target.lessons],
        )
        .map_err(|e| db_err("failed to clear reembed state", &e))?;
        let active = active_meta(conn)?;
        if active.is_some_and(|m| m.spec == current && m.tables == target) {
            return Ok(false);
        }
        record_active(conn, &current, &target, "reembed")?;
        Ok(true)
    })?;
    report.active_tables = target;
    report.build_started_at = None;

    if opts.drop_old {
        report.dropped = drop_inactive_tables(db, &target)?;
    }
    Ok(report)
}

/// How far a kind's active vector table is out of step with its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexGap {
    /// Which rows.
    pub kind: Kind,
    /// Stored rows with no vector.
    pub missing: u64,
    /// Vectors whose row no longer exists.
    pub orphaned: u64,
}

/// Compare each kind's rows with its vectors in `tables`, one query per kind.
///
/// Rows can lack vectors when an older Nellie, which does not know about the
/// switch to new tables, kept writing after `nellie reembed` switched.
///
/// # Errors
///
/// Returns an error if a query fails.
pub fn index_gaps(conn: &Connection, tables: &VectorTables) -> Result<Vec<IndexGap>> {
    Kind::ALL
        .iter()
        .map(|&kind| {
            let src = kind.source_table();
            let vec = kind.vector_table(tables);
            // A lesson in a section table is missing when it has no section.
            let owner = owner_column(vec);
            let (missing, orphaned): (i64, i64) = conn
                .query_row(
                    &format!(
                        "SELECT (SELECT COUNT(*) FROM {src} WHERE id NOT IN (SELECT {owner} FROM {vec})),
                                (SELECT COUNT(*) FROM {vec} WHERE {owner} NOT IN (SELECT id FROM {src}))"
                    ),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|e| db_err("failed to compare index with rows", &e))?;
            Ok(IndexGap {
                kind,
                missing: u64::try_from(missing).unwrap_or(0),
                orphaned: u64::try_from(orphaned).unwrap_or(0),
            })
        })
        .collect()
}

/// Remove vectors in `tables` whose row no longer exists. Returns how many
/// were removed.
///
/// # Errors
///
/// Returns an error if the delete fails.
pub fn remove_orphaned_vectors(db: &Database, tables: &VectorTables) -> Result<u64> {
    Kind::ALL.iter().try_fold(0, |n, &kind| {
        Ok(n + remove_orphans(db, kind, kind.vector_table(tables))?)
    })
}

/// Embed every row that has no vector in `tables`, using the same text as
/// the insert paths. Safe while the server is writing: rows that gain a
/// vector or are deleted meanwhile are skipped.
///
/// # Errors
///
/// Returns an error if embedding or storage fails.
pub async fn embed_missing<E: Embedder>(
    db: &Database,
    embedder: &E,
    tables: &VectorTables,
    opts: &ReembedOptions,
) -> Result<Vec<(Kind, KindReport)>> {
    let mut reports = Vec::with_capacity(3);
    for kind in Kind::ALL {
        let table = kind.vector_table(tables);
        let report = reembed_kind(db, embedder, kind, table, table, opts, &mut |_| {}).await?;
        reports.push((kind, report));
    }
    Ok(reports)
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
    use rusqlite::OptionalExtension;
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
                let mut lesson =
                    LessonRecord::new(format!("Lesson {i}"), format!("Body {i}"), vec![]);
                // Written well before any reembed run starts.
                lesson.created_at -= 1000;
                lesson.updated_at -= 1000;
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
            switch: true,
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

    /// The vector of a row (section 0 for a lesson in a section table).
    fn vector(db: &Database, table: &str, id: &Value) -> Option<Vec<f32>> {
        let key = match id {
            Value::Text(lesson) if is_section_table(table) => Value::Text(section_id(lesson, 0)),
            other => other.clone(),
        };
        db.with_conn(|conn| {
            let blob: Option<Vec<u8>> = conn
                .query_row(
                    &format!("SELECT embedding FROM {table} WHERE id = ?"),
                    [key],
                    |r| r.get(0),
                )
                .optional()
                .unwrap();
            Ok(blob.map(|b| {
                b.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            }))
        })
        .unwrap()
    }

    /// Every stored row has exactly one vector in `tables` and nothing else.
    fn assert_index_matches(db: &Database, tables: &VectorTables) {
        let gaps = db.with_conn(|conn| index_gaps(conn, tables)).unwrap();
        for gap in gaps {
            assert_eq!((gap.missing, gap.orphaned), (0, 0), "{gap:?}");
        }
    }

    /// Writes the way an older Nellie does: no lock, no `embedding_meta`,
    /// vectors only in the legacy tables, own connection.
    fn old_server_conn(dir: &Path) -> Connection {
        crate::storage::init_sqlite_vec();
        Connection::open(dir.join("nellie.db")).unwrap()
    }

    fn old_server_add_lesson(conn: &Connection, title: &str) -> LessonRecord {
        let lesson = LessonRecord::new(title, "written by the old server", vec![]);
        insert_lesson(conn, &lesson).unwrap();
        let blob: Vec<u8> = placeholder_embedding("old")
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        conn.execute(
            "INSERT INTO lesson_embeddings (id, embedding) VALUES (?, ?)",
            rusqlite::params![lesson.id, blob],
        )
        .unwrap();
        lesson
    }

    #[tokio::test]
    async fn prebuild_runs_beside_an_old_server_and_final_run_catches_up() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 20);

        // Pre-build while an older server keeps writing on its own connection.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let dir = dir.path().to_path_buf();
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let conn = old_server_conn(&dir);
                let mut n = 0;
                while (!stop.load(Ordering::SeqCst) || n < 5) && n < 200 {
                    old_server_add_lesson(&conn, &format!("Concurrent {n}"));
                    n += 1;
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                n
            })
        };
        let mut o = opts();
        o.switch = false;
        let pre = run_reembed(&db, &FakeEmbedder::ok(), &o, &mut |_| {})
            .await
            .unwrap();
        stop.store(true, Ordering::SeqCst);
        let concurrent = writer.join().unwrap();
        assert!(!pre.switched);
        let started = pre.build_started_at.unwrap();
        // Not switched; the old server's tables are still active.
        assert_eq!(db.with_conn(active_tables).unwrap(), LEGACY_TABLES);
        let recorded: i64 = db
            .with_conn(|conn| {
                Ok(conn
                    .query_row("SELECT started_at FROM reembed_state", [], |r| r.get(0))
                    .unwrap())
            })
            .unwrap();
        assert_eq!(recorded, started);

        // More changes by the old server after the pre-build: insert, delete,
        // edit, and a re-indexed code file.
        let old = old_server_conn(dir.path());
        let lessons = db.with_conn(crate::storage::list_lessons).unwrap();
        let deleted = lessons.iter().find(|l| l.title == "Lesson 3").unwrap();
        let mut edited = lessons
            .iter()
            .find(|l| l.title == "Lesson 4")
            .unwrap()
            .clone();
        crate::storage::delete_lesson(&old, &deleted.id).unwrap();
        edited.content = "edited while the old server ran".to_string();
        crate::storage::update_lesson(&old, &edited).unwrap();
        let added = old_server_add_lesson(&old, "Added after pre-build");
        let checkpoint = CheckpointRecord::new("agent", "new checkpoint", serde_json::json!({}));
        insert_checkpoint(&old, &checkpoint).unwrap();
        let old_chunk: i64 = old
            .query_row("SELECT id FROM chunks", [], |r| r.get(0))
            .unwrap();
        old.execute("DELETE FROM chunks WHERE file_path = '/src/a.rs'", [])
            .unwrap();
        let new_chunk = insert_chunk(
            &old,
            &ChunkRecord::new("/src/a.rs", 0, 1, 3, "fn a() { changed }", "hash2"),
        )
        .unwrap();
        assert_ne!(old_chunk, new_chunk, "changed files get fresh chunk ids");
        drop(old);

        // A second pre-build keeps the original start time.
        let again = run_reembed(&db, &FakeEmbedder::ok(), &o, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(again.build_started_at, Some(started));

        // Final run, Nellie stopped: catch up and switch.
        let report = run_reembed(&db, &FakeEmbedder::ok(), &opts(), &mut |_| {})
            .await
            .unwrap();
        assert!(report.switched);
        assert_eq!(report.lessons.orphans_removed, 1);
        // The edited lesson, plus lessons the old server added after the
        // start that a pre-build already embedded (re-embedding them is the
        // conservative choice: they too were written after the start).
        assert!(
            (1..=concurrent + 2).contains(&report.lessons.refreshed),
            "refreshed {} with {concurrent} concurrent inserts",
            report.lessons.refreshed
        );
        assert_eq!(report.chunks.orphans_removed, 1);
        assert_eq!(db.with_conn(active_tables).unwrap(), CURRENT_TABLES);
        assert_index_matches(&db, &CURRENT_TABLES);
        assert_eq!(rows(&db, CURRENT_TABLES.lessons), 20 - 1 + concurrent + 1);
        assert_eq!(rows(&db, CURRENT_TABLES.checkpoints), 2);
        assert_eq!(rows(&db, CURRENT_TABLES.chunks), 1);

        // Vectors reflect the current text.
        let text =
            |l: &LessonRecord| placeholder_embedding(&lesson_embedding_text(&l.title, &l.content));
        let lesson_vec =
            |id: &str| vector(&db, CURRENT_TABLES.lessons, &Value::Text(id.to_string()));
        assert_eq!(lesson_vec(&edited.id).unwrap(), text(&edited));
        assert_eq!(lesson_vec(&added.id).unwrap(), text(&added));
        assert!(lesson_vec(&deleted.id).is_none());
        assert_eq!(
            vector(&db, CURRENT_TABLES.chunks, &Value::Integer(new_chunk)).unwrap(),
            placeholder_embedding(&chunk_embedding_text("fn a() { changed }"))
        );
        // Build state is cleared once switched.
        assert_eq!(
            db.with_conn(|conn| count(conn, "SELECT COUNT(*) FROM reembed_state"))
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn no_switch_leaves_an_up_to_date_index_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("nellie.db")).unwrap();
        init_storage(&db).unwrap();
        db.with_conn(|conn| insert_lesson(conn, &LessonRecord::new("t", "c", vec![])))
            .unwrap();
        let mut o = opts();
        o.switch = false;
        let report = run_reembed(&db, &FakeEmbedder::ok(), &o, &mut |_| {})
            .await
            .unwrap();
        assert!(report.build_started_at.is_none());
        assert_eq!(report.lessons.embedded, 0);
        assert_eq!(rows(&db, CURRENT_TABLES.lessons), 0);
    }

    /// Adds a lesson the first time it embeds a chunk: a server writing
    /// during the switching run.
    struct WriterEmbedder {
        db: Database,
        written: std::sync::atomic::AtomicBool,
    }

    impl Embedder for WriterEmbedder {
        fn embed(&self, texts: Vec<String>) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send {
            if texts.iter().any(|t| t.contains("fn a()"))
                && !self.written.swap(true, Ordering::SeqCst)
            {
                self.db
                    .with_conn(|conn| insert_lesson(conn, &LessonRecord::new("late", "x", vec![])))
                    .unwrap();
            }
            async move { Ok(texts.iter().map(|t| placeholder_embedding(t)).collect()) }
        }
    }

    #[tokio::test]
    async fn rows_added_during_the_switching_run_block_the_switch() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 3);
        let embedder = WriterEmbedder {
            db: db.clone(),
            written: std::sync::atomic::AtomicBool::new(false),
        };
        let err = run_reembed(&db, &embedder, &opts(), &mut |_| {})
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("lessons changed while reembedding"), "{err}");
        assert_eq!(db.with_conn(active_tables).unwrap(), LEGACY_TABLES);
        // A rerun picks the row up and switches.
        let report = run_reembed(&db, &embedder, &opts(), &mut |_| {})
            .await
            .unwrap();
        assert!(report.switched);
        assert_index_matches(&db, &CURRENT_TABLES);
    }

    #[tokio::test]
    async fn startup_repair_fills_rows_an_old_server_wrote_after_the_switch() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 5);
        run_reembed(&db, &FakeEmbedder::ok(), &opts(), &mut |_| {})
            .await
            .unwrap();
        // Nothing to do: every count is zero.
        assert_index_matches(&db, &CURRENT_TABLES);

        // An older server was still running and kept writing to the legacy
        // tables after the switch.
        let old = old_server_conn(dir.path());
        let added = old_server_add_lesson(&old, "after switch");
        let removed = db
            .with_conn(crate::storage::list_lessons)
            .unwrap()
            .into_iter()
            .find(|l| l.title == "Lesson 0")
            .unwrap();
        // The old server deletes the row but not the new tables' vectors.
        old.execute("DELETE FROM lessons WHERE id = ?", [&removed.id])
            .unwrap();
        insert_checkpoint(
            &old,
            &CheckpointRecord::new("agent", "after switch", serde_json::json!({})),
        )
        .unwrap();
        drop(old);

        let gaps = db
            .with_conn(|conn| index_gaps(conn, &CURRENT_TABLES))
            .unwrap();
        let by_kind = |k: Kind| gaps.iter().find(|g| g.kind == k).copied().unwrap();
        assert_eq!(by_kind(Kind::Lessons).missing, 1);
        assert_eq!(by_kind(Kind::Lessons).orphaned, 1);
        assert_eq!(by_kind(Kind::Checkpoints).missing, 1);
        assert_eq!(
            by_kind(Kind::Chunks),
            IndexGap {
                kind: Kind::Chunks,
                missing: 0,
                orphaned: 0
            }
        );

        assert_eq!(remove_orphaned_vectors(&db, &CURRENT_TABLES).unwrap(), 1);
        let reports = embed_missing(&db, &FakeEmbedder::ok(), &CURRENT_TABLES, &opts())
            .await
            .unwrap();
        let embedded: u64 = reports.iter().map(|(_, r)| r.embedded).sum();
        assert_eq!(embedded, 2);
        assert_index_matches(&db, &CURRENT_TABLES);
        assert_eq!(
            vector(&db, CURRENT_TABLES.lessons, &Value::Text(added.id.clone())).unwrap(),
            placeholder_embedding(&lesson_embedding_text(&added.title, &added.content))
        );
    }

    /// Six 60-word paragraphs: two sections with the test embedder's
    /// word-count tokenizer.
    fn long_content(tag: &str) -> String {
        (0..6)
            .map(|p| {
                (0..60)
                    .map(|w| format!("{tag}{p}w{w}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn sections_of(db: &Database, lesson_id: &str) -> u64 {
        db.with_conn(|conn| {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {} WHERE lesson_id = ?",
                        CURRENT_TABLES.lessons
                    ),
                    [lesson_id],
                    |r| r.get(0),
                )
                .unwrap();
            Ok(u64::try_from(n).unwrap())
        })
        .unwrap()
    }

    #[tokio::test]
    async fn long_lessons_get_one_vector_per_section() {
        let dir = tempfile::tempdir().unwrap();
        let db = legacy_db(dir.path(), 3);
        let long = LessonRecord::new("Long lesson", long_content("x"), vec![]);
        db.with_conn(|conn| insert_lesson(conn, &long)).unwrap();
        let texts = FakeEmbedder::ok()
            .lesson_texts(&long.title, &long.content)
            .unwrap();
        assert_eq!(texts.len(), 2);

        let report = run_reembed(&db, &FakeEmbedder::ok(), &opts(), &mut |_| {})
            .await
            .unwrap();
        assert!(report.switched);
        assert_eq!(report.lessons.embedded, 4);
        assert_eq!(rows(&db, CURRENT_TABLES.lessons), 3 + 2);
        assert_eq!(sections_of(&db, &long.id), 2);
        assert_index_matches(&db, &CURRENT_TABLES);

        // A query matching the second section finds the lesson, at that
        // section's distance, once.
        let query = placeholder_embedding(&texts[1]);
        let hits = db
            .with_conn(|conn| search_lessons_by_embedding(conn, &query, 10))
            .unwrap();
        assert_eq!(hits[0].record.id, long.id);
        assert!(hits[0].distance < 1e-3);
        assert_eq!(hits.iter().filter(|h| h.record.id == long.id).count(), 1);
        assert_eq!(hits.len(), 4);

        // Startup repair: a lesson with no section rows is missing, and is
        // embedded again with all its sections.
        db.with_conn(|conn| {
            conn.execute(
                &format!("DELETE FROM {} WHERE lesson_id = ?", CURRENT_TABLES.lessons),
                [&long.id],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let gaps = db
            .with_conn(|conn| index_gaps(conn, &CURRENT_TABLES))
            .unwrap();
        assert_eq!(gaps[0].kind, Kind::Lessons);
        assert_eq!((gaps[0].missing, gaps[0].orphaned), (1, 0));
        embed_missing(&db, &FakeEmbedder::ok(), &CURRENT_TABLES, &opts())
            .await
            .unwrap();
        assert_eq!(sections_of(&db, &long.id), 2);
        assert_index_matches(&db, &CURRENT_TABLES);

        // Orphaned sections (a row deleted behind the index's back) are
        // counted per vector and all removed.
        db.with_conn(|conn| {
            conn.execute("DELETE FROM lessons WHERE id = ?", [&long.id])
                .unwrap();
            Ok(())
        })
        .unwrap();
        let gaps = db
            .with_conn(|conn| index_gaps(conn, &CURRENT_TABLES))
            .unwrap();
        assert_eq!((gaps[0].missing, gaps[0].orphaned), (0, 2));
        assert_eq!(remove_orphaned_vectors(&db, &CURRENT_TABLES).unwrap(), 2);
        assert_index_matches(&db, &CURRENT_TABLES);
    }

    #[tokio::test]
    async fn deleting_or_editing_a_lesson_removes_all_its_sections() {
        let db = Database::open_in_memory().unwrap();
        init_storage(&db).unwrap();
        let mut long = LessonRecord::new("Long lesson", long_content("y"), vec![]);
        let other = LessonRecord::new("Other", "short", vec![]);
        db.with_conn(|conn| {
            insert_lesson(conn, &long)?;
            insert_lesson(conn, &other)
        })
        .unwrap();
        embed_missing(&db, &FakeEmbedder::ok(), &CURRENT_TABLES, &opts())
            .await
            .unwrap();
        assert_eq!(sections_of(&db, &long.id), 2);

        // Storing new vectors replaces every old section.
        db.with_conn(|conn| {
            crate::storage::store_lesson_embeddings(
                conn,
                &long.id,
                &[
                    placeholder_embedding("a"),
                    placeholder_embedding("b"),
                    placeholder_embedding("c"),
                ],
            )
        })
        .unwrap();
        assert_eq!(sections_of(&db, &long.id), 3);
        db.with_conn(|conn| {
            crate::storage::store_lesson_embedding(conn, &long.id, &placeholder_embedding("d"))
        })
        .unwrap();
        assert_eq!(sections_of(&db, &long.id), 1);

        // A tag-only edit keeps the vectors; a text edit drops them all.
        long.tags = vec!["tag".to_string()];
        db.with_conn(|conn| crate::storage::update_lesson(conn, &long))
            .unwrap();
        assert_eq!(sections_of(&db, &long.id), 1);
        long.content = long_content("z");
        db.with_conn(|conn| crate::storage::update_lesson(conn, &long))
            .unwrap();
        assert_eq!(sections_of(&db, &long.id), 0);
        embed_missing(&db, &FakeEmbedder::ok(), &CURRENT_TABLES, &opts())
            .await
            .unwrap();
        assert_eq!(sections_of(&db, &long.id), 2);

        db.with_conn(|conn| crate::storage::delete_lesson(conn, &long.id))
            .unwrap();
        assert_eq!(sections_of(&db, &long.id), 0);
        assert_eq!(sections_of(&db, &other.id), 1);
        assert_index_matches(&db, &CURRENT_TABLES);
    }

    #[tokio::test]
    async fn rebuild_from_whole_lesson_tables_only_embeds_lessons() {
        use crate::embeddings::LESSON_VECTORS_WHOLE;
        use crate::storage::embedding_meta::MINILM256_WHOLE_TABLES;

        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("nellie.db")).unwrap();
        let whole = EmbeddingSpec {
            lesson_vectors: LESSON_VECTORS_WHOLE.to_string(),
            ..EmbeddingSpec::current()
        };
        let long = LessonRecord::new("Long lesson", long_content("w"), vec![]);
        db.with_conn(|conn| {
            migrate(conn)?;
            create_vector_tables(conn, &MINILM256_WHOLE_TABLES)?;
            record_active(conn, &whole, &MINILM256_WHOLE_TABLES, "fresh")?;
            let blob = |t: &str| -> Vec<u8> {
                placeholder_embedding(t)
                    .iter()
                    .flat_map(|f| f.to_le_bytes())
                    .collect()
            };
            for mut lesson in [LessonRecord::new("Short", "body", vec![]), long.clone()] {
                // Written well before the rebuild starts.
                lesson.created_at -= 1000;
                lesson.updated_at -= 1000;
                insert_lesson(conn, &lesson)?;
                conn.execute(
                    "INSERT INTO lesson_embeddings_minilm256 (id, embedding) VALUES (?, ?)",
                    rusqlite::params![lesson.id, blob("whole")],
                )
                .unwrap();
            }
            let checkpoint = CheckpointRecord::new("agent", "cp", serde_json::json!({}));
            insert_checkpoint(conn, &checkpoint)?;
            conn.execute(
                "INSERT INTO checkpoint_embeddings_minilm256 (id, embedding) VALUES (?, ?)",
                rusqlite::params![checkpoint.id, blob("cp")],
            )
            .unwrap();
            let chunk = insert_chunk(
                conn,
                &ChunkRecord::new("/src/a.rs", 0, 1, 3, "fn a() {}", "h"),
            )?;
            conn.execute(
                "INSERT INTO chunk_embeddings_minilm256 (id, embedding) VALUES (?, ?)",
                rusqlite::params![chunk, blob("chunk")],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        init_storage(&db).unwrap();
        // The guard asks for a rebuild.
        assert!(db
            .with_conn(|conn| crate::storage::embedding_meta::check_spec(
                conn,
                Path::new("/nonexistent/tokenizer.json")
            ))
            .unwrap()
            .is_err());

        // Pre-build beside a running server, then the switching run.
        let mut o = opts();
        o.switch = false;
        let pre = run_reembed(&db, &FakeEmbedder::ok(), &o, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(pre.lessons.embedded, 2);
        assert_eq!((pre.checkpoints.embedded, pre.chunks.embedded), (0, 0));
        assert_eq!((pre.checkpoints.skipped, pre.chunks.skipped), (1, 1));
        o.switch = true;
        o.drop_old = true;
        let report = run_reembed(&db, &FakeEmbedder::ok(), &o, &mut |_| {})
            .await
            .unwrap();
        assert!(report.switched);
        assert_eq!(report.lessons.embedded, 0);
        assert_eq!(report.dropped, vec![MINILM256_WHOLE_TABLES.lessons]);
        assert_eq!(db.with_conn(active_tables).unwrap(), CURRENT_TABLES);
        assert_eq!(sections_of(&db, &long.id), 2);
        // Chunk and checkpoint vectors were kept, not re-embedded.
        assert_eq!(rows(&db, CURRENT_TABLES.chunks), 1);
        assert_index_matches(&db, &CURRENT_TABLES);
        assert!(db
            .with_conn(|conn| crate::storage::embedding_meta::check_spec(
                conn,
                Path::new("/nonexistent/tokenizer.json")
            ))
            .unwrap()
            .is_ok());
    }

    #[tokio::test]
    async fn embed_missing_skips_rows_that_got_a_vector_meanwhile() {
        let db = Database::open_in_memory().unwrap();
        init_storage(&db).unwrap();
        let lesson = LessonRecord::new("t", "c", vec![]);
        db.with_conn(|conn| insert_lesson(conn, &lesson)).unwrap();
        // The server stores its own vector after the row was picked up.
        let rows = vec![(
            Value::Text(lesson.id.clone()),
            vec![placeholder_embedding("server")],
        )];
        db.with_conn(|conn| {
            crate::storage::store_lesson_embedding(conn, &lesson.id, &placeholder_embedding("x"))
        })
        .unwrap();
        let inserted = db
            .with_transaction(|conn| {
                insert_vectors(conn, Kind::Lessons, CURRENT_TABLES.lessons, &rows)
            })
            .unwrap();
        assert_eq!(inserted, 0);
        // Rows deleted meanwhile are skipped too.
        let gone = vec![(
            Value::Text("deleted".to_string()),
            vec![placeholder_embedding("x")],
        )];
        let inserted = db
            .with_transaction(|conn| {
                insert_vectors(conn, Kind::Lessons, CURRENT_TABLES.lessons, &gone)
            })
            .unwrap();
        assert_eq!(inserted, 0);
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
