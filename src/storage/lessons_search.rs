//! Lesson semantic search.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::embedding_meta::{active_tables, ensure_vector_tables};
use super::models::{LessonRecord, SearchResult};
use crate::error::StorageError;
use crate::Result;

/// Name of the active lesson vector table (see `embedding_meta`).
fn lesson_vec_table(conn: &Connection) -> Result<&'static str> {
    Ok(active_tables(conn)?.lessons)
}

/// Initialize the active vector tables (chunks, lessons, checkpoints).
///
/// # Errors
///
/// Returns an error if the tables cannot be created.
pub fn init_lesson_vectors(conn: &Connection) -> Result<()> {
    ensure_vector_tables(conn)
}

/// Store lesson embedding.
///
/// # Errors
///
/// Returns an error if the embedding cannot be stored.
pub fn store_lesson_embedding(conn: &Connection, lesson_id: &str, embedding: &[f32]) -> Result<()> {
    let table = lesson_vec_table(conn)?;

    // Delete old embedding if exists
    conn.execute(&format!("DELETE FROM {table} WHERE id = ?"), [lesson_id])
        .ok();

    // Insert new embedding
    let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    conn.execute(
        &format!("INSERT INTO {table} (id, embedding) VALUES (?, ?)"),
        rusqlite::params![lesson_id, blob],
    )
    .map_err(|e| StorageError::Vector(format!("failed to store lesson embedding: {e}")))?;

    Ok(())
}

/// Search lessons by embedding similarity.
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_lessons_by_embedding(
    conn: &Connection,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<SearchResult<LessonRecord>>> {
    let blob: Vec<u8> = query_embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();

    let table = lesson_vec_table(conn)?;
    let sql = format!(
        "SELECT id, distance FROM {table} WHERE embedding MATCH ? ORDER BY distance LIMIT ?"
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::Vector(format!("failed to prepare search: {e}")))?;

    let candidates: Vec<(String, f32)> = stmt
        .query_map(
            rusqlite::params![blob, i64::try_from(limit).unwrap_or(10)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| StorageError::Vector(e.to_string()))?
        .filter_map(std::result::Result::ok)
        .collect();

    let mut results = Vec::new();
    for (id, distance) in candidates {
        if let Ok(lesson) = super::lessons::get_lesson(conn, &id) {
            results.push(SearchResult::new(lesson, distance));
        }
    }

    Ok(results)
}

/// Reciprocal Rank Fusion constant `k` for lesson search.
///
/// A lesson's fused score is the sum over retrievers of `1 / (k + rank)`
/// (rank starting at 1). Checked on a known-answer eval with a held-out
/// split: with candidate lists of depth 10, any `k` from 10 to 100 ranks
/// identically, and a very small `k` that looked better on the tuning half did
/// not hold up on the held-out half, so the customary 60 is kept.
pub const LESSON_RRF_K: f32 = 60.0;

/// Minimum depth of each candidate list (keyword and vector) before fusion.
///
/// Each retriever contributes `max(limit, LESSON_MIN_CANDIDATES)` candidates.
/// Deeper lists were measured to hurt with `k = 60`: lessons that sit low in
/// both lists start to outrank the top hit of one list.
pub const LESSON_MIN_CANDIDATES: usize = 10;

/// Upper bound on results from one lesson search (matches the largest
/// configurable REST result limit).
const LESSON_MAX_RESULTS: usize = 100_000;

/// Default cap on a lesson-search `limit`.
///
/// Applies when no maximum result limit is configured (`--max-result-limit` /
/// `NELLIE_MAX_RESULT_LIMIT`). Shared by REST and MCP so both entry points
/// enforce the same limit.
pub const LESSON_SEARCH_DEFAULT_CAP: usize = 100;

/// Clamp a requested lesson-search limit to `1..=cap`, where `cap` is the
/// configured maximum result limit or [`LESSON_SEARCH_DEFAULT_CAP`].
#[must_use]
pub fn lesson_search_limit(requested: usize, max_result_limit: Option<u32>) -> usize {
    let cap = max_result_limit.map_or(LESSON_SEARCH_DEFAULT_CAP, |m| m as usize);
    requested.clamp(1, cap.max(1))
}

/// Maximum number of query words passed to the full-text index.
///
/// Only the FIRST 32 distinct, non-stopword words of a query reach keyword search; anything
/// later is seen by the vector side only. A caller that builds a query from
/// several parts (e.g. the current message plus earlier context) should put
/// the most important part first.
const MAX_KEYWORD_TERMS: usize = 32;

/// Common English words left out of keyword queries. A query made only of
/// these runs as vector search alone.
const STOPWORDS: &[&str] = &[
    "a", "about", "after", "again", "all", "also", "an", "and", "any", "are", "as", "at", "be",
    "before", "but", "by", "can", "could", "did", "do", "does", "for", "from", "has", "have", "he",
    "how", "i", "if", "in", "into", "is", "it", "its", "just", "me", "my", "no", "not", "of", "on",
    "or", "our", "over", "she", "should", "so", "some", "than", "that", "the", "their", "them",
    "then", "there", "these", "they", "this", "to", "too", "under", "very", "was", "we", "were",
    "what", "when", "where", "which", "who", "why", "will", "with", "would", "you", "your",
];

/// Build an FTS5 `MATCH` expression from free text, or `None` if nothing
/// searchable is left.
///
/// Every whitespace-separated word becomes a double-quoted FTS5 string (with
/// embedded quotes doubled), and the strings are joined with `OR`. Inside a
/// quoted string FTS5 treats everything as text, so operators (`AND`, `OR`,
/// `NOT`, `NEAR`), column filters (`col:`), prefix stars, `^`, `-`, `+` and
/// parentheses in the query are searched for literally (or dropped by the
/// tokenizer) and cannot change the query's meaning or make it fail to parse.
/// A word like `msg_ab12` or `error[E0999]` becomes a phrase of its tokens,
/// so identifiers still match as a unit. Words without a letter or digit and
/// common stopwords are skipped; at most [`MAX_KEYWORD_TERMS`] words are used.
#[must_use]
pub fn lesson_keyword_query(query: &str) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|word| {
            let bare: String = word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            !bare.is_empty() && !STOPWORDS.contains(&bare.as_str())
        })
        .filter(|word| seen.insert(word.to_lowercase()))
        .take(MAX_KEYWORD_TERMS)
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect();

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

/// Lesson ids ranked by BM25 over title, content and tags (best first).
///
/// Returns an empty list when the query has no searchable words.
///
/// # Errors
///
/// Returns an error if the full-text query fails.
pub fn search_lessons_by_keyword(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let Some(expr) = lesson_keyword_query(query) else {
        return Ok(Vec::new());
    };

    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let ids = retry_on_schema_change(conn, || {
        conn.prepare("SELECT id FROM lessons_fts WHERE lessons_fts MATCH ? ORDER BY rank LIMIT ?")?
            .query_map(rusqlite::params![expr, limit], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
    })
    .map_err(|e| StorageError::Database(format!("keyword search failed: {e}")))?;

    Ok(ids)
}

/// One result of [`search_lessons_hybrid`].
///
/// Serialises as `{record, score, similarity, distance, keyword_rank}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonSearchHit {
    /// The matching lesson.
    pub record: LessonRecord,

    /// Fused relevance in `[0, 1]`: the lesson's Reciprocal Rank Fusion sum
    /// divided by the largest sum possible, `2 / (k + 1)` (first in both the
    /// keyword and the vector ranking). Results are ordered by it, and since
    /// the denominator is fixed it can be compared across queries. First in
    /// both rankings scores `1.0`; first in one ranking and absent from the
    /// other scores about `0.5`; when only one ranking runs (no embedding
    /// service, or a query with no searchable words) the top hit scores `0.5`.
    pub score: f32,

    /// Cosine similarity between the query and lesson vectors (`-1..1`),
    /// or `None` if the lesson was found by keyword search only.
    pub similarity: Option<f32>,

    /// L2 distance between the query and lesson vectors (`0..2`), or `None`
    /// if the lesson was found by keyword search only.
    pub distance: Option<f32>,

    /// 1-based position in the keyword (BM25) ranking, or `None` if the
    /// lesson was found by vector search only.
    pub keyword_rank: Option<usize>,
}

/// Run `f` (a statement that uses `lessons_fts`, directly or through the
/// triggers on `lessons`), retrying if it fails with `SQLITE_SCHEMA`.
///
/// FTS5 connects its table the first time a connection uses it. If another
/// process changed the schema since this connection last read it (for
/// example `nellie reembed` creating tables), the connect fails with
/// `SQLITE_SCHEMA`, which SQLite's automatic re-prepare does not cover, and
/// it keeps failing until the connection reloads its schema. Reading
/// `sqlite_master` reloads it.
///
/// # Errors
///
/// Returns the error of the last attempt.
pub fn retry_on_schema_change<T>(
    conn: &Connection,
    mut f: impl FnMut() -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    const ATTEMPTS: usize = 8;
    let mut attempt = 1;
    loop {
        match f() {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::SchemaChanged && attempt < ATTEMPTS =>
            {
                attempt += 1;
                let _ = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
                    row.get::<_, i64>(0)
                });
            }
            result => return result,
        }
    }
}

/// Fusion state of one lesson: RRF sum, vector distance, keyword rank, and
/// the order first seen (vector list first) to break ties deterministically.
struct Fused {
    sum: f32,
    distance: Option<f32>,
    keyword_rank: Option<usize>,
    order: usize,
}

/// Search lessons by fusing keyword (BM25) and vector rankings.
///
/// Each retriever returns its top `max(limit, LESSON_MIN_CANDIDATES)` lessons;
/// the union is ranked by Reciprocal Rank Fusion with [`LESSON_RRF_K`]. Pass
/// `query_embedding = None` to rank by keywords alone; a query with no
/// searchable words (empty or only stopwords) is ranked by vectors alone.
/// See [`LessonSearchHit`] for what each result field means.
///
/// # Errors
///
/// Returns an error if either underlying search fails.
pub fn search_lessons_hybrid(
    conn: &Connection,
    query: &str,
    query_embedding: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<LessonSearchHit>> {
    // Callers pass a request-supplied limit; bound it so neither the SQL
    // depth nor the result allocation can be driven arbitrarily high.
    let limit = limit.min(LESSON_MAX_RESULTS);
    let depth = limit.max(LESSON_MIN_CANDIDATES);
    let keyword = search_lessons_by_keyword(conn, query, depth)?;
    let vector: Vec<(String, f32)> = match query_embedding {
        Some(embedding) => search_lessons_by_embedding(conn, embedding, depth)?
            .into_iter()
            .map(|r| (r.record.id, r.distance))
            .collect(),
        None => Vec::new(),
    };

    #[allow(clippy::cast_precision_loss)]
    let rrf = |rank: usize| 1.0 / (LESSON_RRF_K + rank as f32);
    let mut fused: HashMap<&str, Fused> = HashMap::new();
    for (i, (id, distance)) in vector.iter().enumerate() {
        let order = fused.len();
        let entry = fused.entry(id.as_str()).or_insert(Fused {
            sum: 0.0,
            distance: None,
            keyword_rank: None,
            order,
        });
        entry.sum += rrf(i + 1);
        entry.distance = Some(*distance);
    }
    for (i, id) in keyword.iter().enumerate() {
        let order = fused.len();
        let entry = fused.entry(id.as_str()).or_insert(Fused {
            sum: 0.0,
            distance: None,
            keyword_rank: None,
            order,
        });
        entry.sum += rrf(i + 1);
        entry.keyword_rank = Some(i + 1);
    }

    let mut fused: Vec<(&str, Fused)> = fused.into_iter().collect();
    fused.sort_by(|a, b| b.1.sum.total_cmp(&a.1.sum).then(a.1.order.cmp(&b.1.order)));
    fused.truncate(limit);

    let best = 2.0 * rrf(1);
    let mut results = Vec::new();
    for (id, f) in fused {
        let Ok(record) = super::lessons::get_lesson(conn, id) else {
            continue;
        };
        results.push(LessonSearchHit {
            record,
            score: (f.sum / best).clamp(0.0, 1.0),
            // Stored vectors are L2-normalised, so cos = 1 - d^2 / 2.
            similarity: f.distance.map(|d| 1.0 - d * d / 2.0),
            distance: f.distance,
            keyword_rank: f.keyword_rank,
        });
    }

    Ok(results)
}

/// Search lessons by text match (LIKE substring).
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_lessons_by_text(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<LessonRecord>> {
    let pattern = format!("%{query}%");

    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
             FROM lessons
             WHERE title LIKE ? OR content LIKE ?
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let lessons = stmt
        .query_map(
            rusqlite::params![&pattern, &pattern, i64::try_from(limit).unwrap_or(10)],
            |row| {
                let tags_json: String = row.get(3)?;
                let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

                Ok(LessonRecord {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    tags,
                    severity: row.get(4)?,
                    agent: row.get(5)?,
                    repo: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    embedding: None,
                })
            },
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for lesson in lessons {
        result.push(lesson.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Search lessons by tag.
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_lessons_by_tag(conn: &Connection, tag: &str) -> Result<Vec<LessonRecord>> {
    // Tags are stored as JSON array, search with LIKE
    let pattern = format!("%\"{tag}\"%");

    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
             FROM lessons
             WHERE tags LIKE ?
             ORDER BY created_at DESC",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let lessons = stmt
        .query_map([pattern], |row| {
            let tags_json: String = row.get(3)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            Ok(LessonRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                tags,
                severity: row.get(4)?,
                agent: row.get(5)?,
                repo: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                embedding: None,
            })
        })
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for lesson in lessons {
        result.push(lesson.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Search lessons by multiple tags (AND logic - must have all tags).
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_lessons_by_tags_all(conn: &Connection, tags: &[&str]) -> Result<Vec<LessonRecord>> {
    if tags.is_empty() {
        return Ok(Vec::new());
    }

    // Build WHERE clause for all tags
    let where_clauses: Vec<String> = tags
        .iter()
        .map(|tag| format!("tags LIKE '%\"{}\"%%'", tag.replace('\'', "''")))
        .collect();
    let where_condition = where_clauses.join(" AND ");

    let sql = format!(
        "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
         FROM lessons
         WHERE {where_condition}
         ORDER BY created_at DESC"
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let lessons = stmt
        .query_map([], |row| {
            let tags_json: String = row.get(3)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            Ok(LessonRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                tags,
                severity: row.get(4)?,
                agent: row.get(5)?,
                repo: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                embedding: None,
            })
        })
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for lesson in lessons {
        result.push(lesson.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Search lessons by multiple tags (OR logic - has any of the tags).
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_lessons_by_tags_any(conn: &Connection, tags: &[&str]) -> Result<Vec<LessonRecord>> {
    if tags.is_empty() {
        return Ok(Vec::new());
    }

    // Build WHERE clause for any tags
    let where_clauses: Vec<String> = tags
        .iter()
        .map(|tag| format!("tags LIKE '%\"{}\"%%'", tag.replace('\'', "''")))
        .collect();
    let where_condition = where_clauses.join(" OR ");

    let sql = format!(
        "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
         FROM lessons
         WHERE {where_condition}
         ORDER BY created_at DESC"
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let lessons = stmt
        .query_map([], |row| {
            let tags_json: String = row.get(3)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            Ok(LessonRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                tags,
                severity: row.get(4)?,
                agent: row.get(5)?,
                repo: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                embedding: None,
            })
        })
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for lesson in lessons {
        result.push(lesson.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Get all unique tags with their counts.
///
/// # Errors
///
/// Returns an error if the query fails.
pub fn get_all_tags(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn
        .prepare("SELECT tags FROM lessons")
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut tag_counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    let lessons = stmt
        .query_map([], |row| {
            let tags_json: String = row.get(0)?;
            Ok(tags_json)
        })
        .map_err(|e| StorageError::Database(e.to_string()))?;

    for lesson_result in lessons {
        let tags_json = lesson_result.map_err(|e| StorageError::Database(e.to_string()))?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        for tag in tags {
            *tag_counts.entry(tag).or_insert(0) += 1;
        }
    }

    let mut result: Vec<(String, i64)> = tag_counts.into_iter().collect();
    result.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Ok(result)
}

/// Filter lessons by tag and severity.
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn filter_lessons_by_tag_and_severity(
    conn: &Connection,
    tag: &str,
    severity: &str,
) -> Result<Vec<LessonRecord>> {
    let pattern = format!("%\"{tag}\"%");

    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
             FROM lessons
             WHERE tags LIKE ? AND severity = ?
             ORDER BY created_at DESC",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let lessons = stmt
        .query_map(rusqlite::params![&pattern, severity], |row| {
            let tags_json: String = row.get(3)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

            Ok(LessonRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                tags,
                severity: row.get(4)?,
                agent: row.get(5)?,
                repo: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                embedding: None,
            })
        })
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for lesson in lessons {
        result.push(lesson.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{insert_lesson, migrate, Database};

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| migrate(conn)).unwrap();
        db
    }

    /// Storage with vector tables, for tests that store embeddings.
    fn setup_vector_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        crate::storage::init_storage(&db).unwrap();
        db
    }

    /// A unit vector pointing mostly along `axis`.
    fn axis_vector(axis: usize) -> Vec<f32> {
        let mut v = vec![0.01_f32; crate::storage::vector::EMBEDDING_DIM];
        v[axis] = 1.0;
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / norm).collect()
    }

    /// Unit vector `axis_vector(a)` tilted towards axis `b` by `weight`.
    fn mixed_vector(a: usize, b: usize, weight: f32) -> Vec<f32> {
        let mut v = axis_vector(a);
        v[b] += weight;
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / norm).collect()
    }

    fn keyword_titles(conn: &Connection, query: &str) -> Vec<String> {
        search_lessons_by_keyword(conn, query, 10)
            .unwrap()
            .iter()
            .map(|id| crate::storage::get_lesson(conn, id).unwrap().title)
            .collect()
    }

    #[test]
    fn keyword_query_quotes_every_word() {
        assert_eq!(
            lesson_keyword_query("cargo build failed").as_deref(),
            Some(r#""cargo" OR "build" OR "failed""#)
        );
        // Operators, column filters, prefix stars and parentheses stay inside
        // quoted strings, so FTS5 never parses them as syntax ("AND" is also
        // a stopword).
        assert_eq!(
            lesson_keyword_query(r#"NEAR(alpha beta) AND title:x* -gamma ^delta"#).as_deref(),
            Some(r#""NEAR(alpha" OR "beta)" OR "title:x*" OR "-gamma" OR "^delta""#)
        );
        // Embedded and unbalanced quotes are doubled.
        assert_eq!(
            lesson_keyword_query(r#"say "hi"#).as_deref(),
            Some(r#""say" OR """hi""#)
        );
        // Stopwords, pure punctuation and repeats are dropped.
        assert_eq!(
            lesson_keyword_query("the Exit 151 -- exit ( )").as_deref(),
            Some(r#""Exit" OR "151""#)
        );
    }

    #[test]
    fn keyword_query_empty_or_stopwords_is_none() {
        for q in ["", "   ", "the and of", "--- ( ) \" '", "* : ^"] {
            assert_eq!(lesson_keyword_query(q), None, "query {q:?}");
        }
    }

    #[test]
    fn keyword_query_caps_term_count() {
        let long: String = (0..200).map(|i| format!("w{i} ")).collect();
        let expr = lesson_keyword_query(&long).unwrap();
        assert_eq!(expr.matches(" OR ").count(), MAX_KEYWORD_TERMS - 1);
    }

    #[test]
    fn adversarial_queries_never_error() {
        let db = setup_db();
        db.with_conn(|conn| {
            use crate::storage::LessonRecord;
            insert_lesson(
                conn,
                &LessonRecord::new("Alpha beta", "gamma delta content", vec![]),
            )?;
            let hostile = [
                "\"",
                "\"\"\"",
                "unbalanced \"quote",
                "a\"b",
                "NEAR(alpha beta, 2)",
                "alpha AND",
                "OR alpha",
                "NOT",
                "alpha NOT beta",
                "-alpha",
                "+alpha",
                "title:alpha",
                "content : alpha",
                "{title content}: alpha",
                "alpha*",
                "*",
                "^alpha",
                "(alpha",
                "alpha)",
                "((()))",
                "'; DROP TABLE lessons; --",
                "\u{0}\u{1}",
                "漢字 émoji 🚀",
                "alpha\tbeta\ngamma",
                "",
                "   ",
                "\t\n",
                "AND OR NOT NEAR",
                "alpha OR",
                "alpha AND beta",
                "NEAR/3 alpha",
                "\"alpha beta\"",
                "alpha:beta:gamma",
                "a-b-c --- ---x",
                "x' OR '1'='1",
                "1; SELECT * FROM lessons_fts; --",
                "SELECT id FROM lessons WHERE 1=1 UNION SELECT name FROM sqlite_master",
                "\u{feff}alpha\u{200b}",
                "Ω≈ç√∫ ÅÍÎÏ ﬁ",
            ];
            let very_long = "alpha \"beta( ".repeat(5000);
            search_lessons_by_keyword(conn, &very_long, 10)?;
            search_lessons_hybrid(conn, &very_long, None, 10)?;
            for q in hostile {
                search_lessons_by_keyword(conn, q, 10)
                    .unwrap_or_else(|e| panic!("query {q:?} failed: {e}"));
                search_lessons_hybrid(conn, q, None, 10)
                    .unwrap_or_else(|e| panic!("query {q:?} failed: {e}"));
            }
            // The table survived and operator words are searched as text.
            assert_eq!(crate::storage::count_lessons(conn)?, 1);
            assert_eq!(keyword_titles(conn, "alpha NOT beta"), vec!["Alpha beta"]);
            assert_eq!(keyword_titles(conn, "-alpha"), vec!["Alpha beta"]);
            assert_eq!(keyword_titles(conn, "NEAR alpha"), vec!["Alpha beta"]);
            // Operator words are plain words: these mean the same as "alpha".
            for q in [
                "alpha OR",
                "alpha AND",
                "OR alpha",
                "NOT alpha",
                "alpha*",
                "^alpha",
                "(alpha)",
            ] {
                assert_eq!(keyword_titles(conn, q), vec!["Alpha beta"], "query {q:?}");
            }
            assert!(keyword_titles(conn, "AND OR NOT NEAR").is_empty());
            // SQL text is searched as words, not executed.
            assert!(keyword_titles(conn, "x' OR '1'='1").is_empty());
            // A word with inner punctuation is a phrase of its tokens.
            assert!(keyword_titles(conn, "NEAR(alpha").is_empty());
            assert_eq!(keyword_titles(conn, "alpha-beta"), vec!["Alpha beta"]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn lesson_writes_survive_schema_changes_by_another_connection() {
        use crate::storage::{count_lessons, LessonRecord};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nellie.db");
        let db = Database::open(&path).unwrap();
        crate::storage::init_storage(&db).unwrap();
        crate::storage::init_sqlite_vec();

        // A second process writes lessons while this one creates tables (as
        // `nellie reembed` does). FTS5 reports SQLITE_SCHEMA when it connects
        // its table on a connection whose schema went stale meanwhile.
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let stop = Arc::clone(&stop);
            let path = path.clone();
            std::thread::spawn(move || {
                let conn = Connection::open(&path).unwrap();
                let mut errors = Vec::new();
                for i in 0..40 {
                    if stop.load(Ordering::SeqCst) && i >= 10 {
                        break;
                    }
                    let lesson = LessonRecord::new(format!("Lesson {i}"), "persimmon", vec![]);
                    if let Err(e) = insert_lesson(&conn, &lesson) {
                        errors.push(e.to_string());
                    }
                    if let Err(e) = search_lessons_by_keyword(&conn, "persimmon", 5) {
                        errors.push(e.to_string());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                (count_lessons(&conn).unwrap(), errors)
            })
        };
        for n in 0..40 {
            db.with_conn(|conn| {
                conn.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE extra_{n} USING vec0(id TEXT PRIMARY KEY, embedding FLOAT[4])"
                ))
                .unwrap();
                Ok(())
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        stop.store(true, Ordering::SeqCst);
        let (inserted, errors) = writer.join().unwrap();
        assert!(
            errors.is_empty(),
            "{} errors: {:?}",
            errors.len(),
            &errors[..errors.len().min(3)]
        );
        assert!(inserted >= 10);
        let found = db
            .with_conn(|conn| search_lessons_by_keyword(conn, "persimmon", 100))
            .unwrap();
        assert_eq!(i64::try_from(found.len()).unwrap(), inserted);
    }

    #[test]
    fn keyword_search_matches_identifiers_and_tags() {
        let db = setup_db();
        db.with_conn(|conn| {
            use crate::storage::LessonRecord;
            insert_lesson(
                conn,
                &LessonRecord::new(
                    "Runner killed",
                    "The job ended with Exit 151 after error[E0999] in msg_ab12cd34.",
                    vec![],
                ),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("Unrelated", "Exit code 1 from a lint step", vec![]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("Tagged", "body", vec!["zebra-crossing".to_string()]),
            )?;

            assert_eq!(keyword_titles(conn, "msg_ab12cd34")[0], "Runner killed");
            assert_eq!(
                keyword_titles(conn, "E0999 missing fields")[0],
                "Runner killed"
            );
            assert_eq!(keyword_titles(conn, "exit 151")[0], "Runner killed");
            assert_eq!(keyword_titles(conn, "zebra"), vec!["Tagged"]);
            // Porter stemming: "crossings" matches "crossing".
            assert_eq!(keyword_titles(conn, "crossings"), vec!["Tagged"]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn keyword_index_follows_insert_update_delete() {
        let db = setup_db();
        db.with_conn(|conn| {
            use crate::storage::{delete_lesson, update_lesson, LessonRecord};
            let mut lesson = LessonRecord::new("Original title", "kiwifruit content", vec![]);
            insert_lesson(conn, &lesson)?;
            assert_eq!(keyword_titles(conn, "kiwifruit"), vec!["Original title"]);

            lesson.title = "Edited title".to_string();
            lesson.content = "papaya content".to_string();
            lesson.tags = vec!["mango".to_string()];
            update_lesson(conn, &lesson)?;
            assert!(keyword_titles(conn, "kiwifruit").is_empty());
            assert_eq!(keyword_titles(conn, "papaya"), vec!["Edited title"]);
            assert_eq!(keyword_titles(conn, "mango"), vec!["Edited title"]);
            assert!(keyword_titles(conn, "original").is_empty());

            delete_lesson(conn, &lesson.id)?;
            assert!(keyword_titles(conn, "papaya").is_empty());
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM lessons_fts", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migration_backfills_existing_lessons() {
        let db = setup_db();
        db.with_conn(|conn| {
            use crate::storage::LessonRecord;
            // Roll the database back to schema v5 with lessons already present.
            conn.execute_batch(
                "DROP TRIGGER lessons_fts_insert;
                 DROP TRIGGER lessons_fts_update;
                 DROP TRIGGER lessons_fts_delete;
                 DROP TABLE lessons_fts;
                 DELETE FROM schema_migrations WHERE version = 6;",
            )
            .unwrap();
            insert_lesson(conn, &LessonRecord::new("Old one", "persimmon", vec![]))?;
            insert_lesson(conn, &LessonRecord::new("Old two", "quince", vec![]))?;

            migrate(conn)?;
            assert_eq!(keyword_titles(conn, "persimmon"), vec!["Old one"]);
            assert_eq!(keyword_titles(conn, "quince"), vec!["Old two"]);
            // Running migrations again changes nothing.
            migrate(conn)?;
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM lessons_fts", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn hybrid_fuses_keyword_and_vector_hits() {
        let db = setup_vector_db();
        db.with_conn(|conn| {
            use crate::storage::LessonRecord;
            let both = LessonRecord::new("Both match", "mentions token qx77zz", vec![]);
            let semantic = LessonRecord::new("Semantic match", "nothing in common", vec![]);
            let keyword = LessonRecord::new("Keyword match", "also mentions qx77zz", vec![]);
            insert_lesson(conn, &both)?;
            store_lesson_embedding(conn, &both.id, &axis_vector(1))?;
            insert_lesson(conn, &semantic)?;
            store_lesson_embedding(conn, &semantic.id, &mixed_vector(1, 2, 0.2))?;
            insert_lesson(conn, &keyword)?;
            store_lesson_embedding(conn, &keyword.id, &axis_vector(300))?;
            // Fillers closer to the query than "Keyword match", so it falls
            // outside the vector candidates and is found by keyword only.
            for i in 0..LESSON_MIN_CANDIDATES {
                let filler = LessonRecord::new(format!("Filler {i}"), "unrelated text", vec![]);
                insert_lesson(conn, &filler)?;
                store_lesson_embedding(conn, &filler.id, &mixed_vector(1, 10 + i, 0.6))?;
            }

            let query = axis_vector(1);
            let results = search_lessons_hybrid(conn, "qx77zz", Some(&query), 10)?;
            let titles: Vec<&str> = results.iter().map(|r| r.record.title.as_str()).collect();
            assert_eq!(titles[0], "Both match", "first in both lists ranks first");
            assert!(titles.contains(&"Semantic match"));
            assert!(titles.contains(&"Keyword match"));

            for r in &results {
                assert!(r.score > 0.0 && r.score <= 1.0, "score {}", r.score);
            }
            assert!(results.windows(2).all(|w| w[0].score >= w[1].score));

            let hit = |title: &str| results.iter().find(|r| r.record.title == title).unwrap();
            // Found by both: cosine similarity and keyword rank are reported.
            let b = hit("Both match");
            assert!((b.similarity.unwrap() - 1.0).abs() < 1e-3);
            assert!(b.distance.unwrap() < 1e-3);
            assert!(b.keyword_rank.is_some());
            // Vector only: no keyword rank.
            let s = hit("Semantic match");
            assert!(s.similarity.unwrap() > 0.9 && s.similarity.unwrap() < 1.0);
            assert_eq!(s.keyword_rank, None);
            // Keyword only: no similarity or distance.
            let k = hit("Keyword match");
            assert_eq!(k.similarity, None);
            assert_eq!(k.distance, None);
            assert!(k.keyword_rank.is_some());

            // Limit is respected.
            assert_eq!(
                search_lessons_hybrid(conn, "qx77zz", Some(&query), 1)?.len(),
                1
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn hybrid_score_is_normalised_rrf() {
        let db = setup_vector_db();
        db.with_conn(|conn| {
            use crate::storage::LessonRecord;
            let lesson = LessonRecord::new("Only lesson", "unique wombat", vec![]);
            insert_lesson(conn, &lesson)?;
            store_lesson_embedding(conn, &lesson.id, &axis_vector(3))?;
            let other = LessonRecord::new("Other lesson", "nothing", vec![]);
            insert_lesson(conn, &other)?;
            store_lesson_embedding(conn, &other.id, &axis_vector(4))?;
            let query = axis_vector(3);
            let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
            let k = LESSON_RRF_K;

            // First in both rankings: 1.0.
            let r = search_lessons_hybrid(conn, "wombat", Some(&query), 5)?;
            assert!(close(r[0].score, 1.0));
            assert_eq!(r[0].keyword_rank, Some(1));
            // Second in the vector ranking only: (1/(k+2)) / (2/(k+1)).
            assert_eq!(r[1].record.title, "Other lesson");
            assert!(close(r[1].score, (k + 1.0) / (2.0 * (k + 2.0))));

            // Keyword ranking ran but found nothing: half credit.
            let r = search_lessons_hybrid(conn, "platypus", Some(&query), 5)?;
            assert!(close(r[0].score, 0.5));
            assert_eq!(r[0].keyword_rank, None);

            // Stopword-only query: vector ranking alone, still out of 2/(k+1).
            let r = search_lessons_hybrid(conn, "the of and", Some(&query), 5)?;
            assert!(close(r[0].score, 0.5));

            // No query vector: keyword ranking alone, no similarity.
            let r = search_lessons_hybrid(conn, "wombat", None, 5)?;
            assert_eq!(r.len(), 1);
            assert!(close(r[0].score, 0.5));
            assert_eq!(r[0].similarity, None);

            // Nothing to search with.
            assert!(search_lessons_hybrid(conn, "the", None, 5)?.is_empty());
            assert!(search_lessons_hybrid(conn, "", None, 5)?.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_text() {
        let db = setup_db();

        db.with_conn(|conn| {
            use crate::storage::LessonRecord;

            insert_lesson(
                conn,
                &LessonRecord::new("Rust Error Handling", "Use Result type for errors", vec![]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("Python Testing", "Use pytest for testing", vec![]),
            )?;

            let results = search_lessons_by_text(conn, "Rust", 10)?;
            assert_eq!(results.len(), 1);
            assert!(results[0].title.contains("Rust"));

            let results = search_lessons_by_text(conn, "testing", 10)?;
            assert_eq!(results.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_tag() {
        let db = setup_db();

        db.with_conn(|conn| {
            use crate::storage::LessonRecord;

            insert_lesson(
                conn,
                &LessonRecord::new("L1", "C1", vec!["rust".to_string(), "errors".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L2", "C2", vec!["python".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L3", "C3", vec!["rust".to_string()]),
            )?;

            let results = search_lessons_by_tag(conn, "rust")?;
            assert_eq!(results.len(), 2);

            let results = search_lessons_by_tag(conn, "python")?;
            assert_eq!(results.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_tags_all() {
        let db = setup_db();

        db.with_conn(|conn| {
            use crate::storage::LessonRecord;

            insert_lesson(
                conn,
                &LessonRecord::new("L1", "C1", vec!["rust".to_string(), "errors".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L2", "C2", vec!["rust".to_string(), "async".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L3", "C3", vec!["rust".to_string()]),
            )?;

            let results = search_lessons_by_tags_all(conn, &["rust", "errors"])?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].title, "L1");

            let results = search_lessons_by_tags_all(conn, &["rust", "async"])?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].title, "L2");

            let results = search_lessons_by_tags_all(conn, &["rust", "missing"])?;
            assert_eq!(results.len(), 0);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_tags_any() {
        let db = setup_db();

        db.with_conn(|conn| {
            use crate::storage::LessonRecord;

            insert_lesson(
                conn,
                &LessonRecord::new("L1", "C1", vec!["rust".to_string(), "errors".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L2", "C2", vec!["python".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L3", "C3", vec!["javascript".to_string()]),
            )?;

            let results = search_lessons_by_tags_any(conn, &["rust", "python"])?;
            assert_eq!(results.len(), 2);

            let results = search_lessons_by_tags_any(conn, &["javascript"])?;
            assert_eq!(results.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_get_all_tags() {
        let db = setup_db();

        db.with_conn(|conn| {
            use crate::storage::LessonRecord;

            insert_lesson(
                conn,
                &LessonRecord::new("L1", "C1", vec!["rust".to_string(), "errors".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L2", "C2", vec!["rust".to_string(), "async".to_string()]),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L3", "C3", vec!["python".to_string()]),
            )?;

            let tags = get_all_tags(conn)?;
            assert_eq!(tags.len(), 4);

            // Find the counts for specific tags
            let rust_count = tags
                .iter()
                .find(|(tag, _)| tag == "rust")
                .map(|(_, count)| *count);
            assert_eq!(rust_count, Some(2));

            let python_count = tags
                .iter()
                .find(|(tag, _)| tag == "python")
                .map(|(_, count)| *count);
            assert_eq!(python_count, Some(1));

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_filter_by_tag_and_severity() {
        let db = setup_db();

        db.with_conn(|conn| {
            use crate::storage::LessonRecord;

            insert_lesson(
                conn,
                &LessonRecord::new("L1", "C1", vec!["rust".to_string()]).with_severity("critical"),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L2", "C2", vec!["rust".to_string()]).with_severity("warning"),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L3", "C3", vec!["python".to_string()])
                    .with_severity("critical"),
            )?;

            let results = filter_lessons_by_tag_and_severity(conn, "rust", "critical")?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].title, "L1");

            let results = filter_lessons_by_tag_and_severity(conn, "rust", "warning")?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].title, "L2");

            let results = filter_lessons_by_tag_and_severity(conn, "python", "critical")?;
            assert_eq!(results.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn lesson_search_limit_uses_configured_cap_or_default() {
        assert_eq!(lesson_search_limit(5, None), 5);
        assert_eq!(lesson_search_limit(0, None), 1);
        assert_eq!(
            lesson_search_limit(1_000_000, None),
            LESSON_SEARCH_DEFAULT_CAP
        );
        assert_eq!(lesson_search_limit(1_000_000, Some(10_000)), 10_000);
        assert_eq!(lesson_search_limit(50, Some(20)), 20);
    }
}
