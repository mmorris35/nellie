//! Semantic search operations.
//!
//! Provides vector similarity search across chunks, lessons, and checkpoints.

use rusqlite::Connection;

use super::models::{ChunkRecord, SearchResult};
use super::vector::search_similar;
use crate::error::StorageError;
use crate::Result;

/// Vector table name for chunk embeddings.
const CHUNK_VEC_TABLE: &str = "chunk_embeddings";

/// Search options for semantic search.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Maximum number of results to return.
    pub limit: usize,

    /// Minimum similarity score (0.0 to 1.0).
    pub min_score: f32,

    /// Filter by programming language.
    pub language: Option<String>,

    /// Filter by file path pattern.
    pub path_pattern: Option<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            min_score: 0.0,
            language: None,
            path_pattern: None,
        }
    }
}

impl SearchOptions {
    /// Create new search options with limit.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Default::default()
        }
    }

    /// Set minimum similarity score.
    #[must_use]
    pub fn with_min_score(mut self, score: f32) -> Self {
        self.min_score = score.clamp(0.0, 1.0);
        self
    }

    /// Filter by language.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Filter by file path pattern (SQL LIKE).
    #[must_use]
    pub fn with_path_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.path_pattern = Some(pattern.into());
        self
    }
}

/// Search for similar code chunks.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `query_embedding` - The query vector
/// * `options` - Search options (limit, filters, etc.)
///
/// # Returns
///
/// Vector of search results with chunks and scores.
///
/// # Errors
///
/// Returns an error if the search fails.
pub fn search_chunks(
    conn: &Connection,
    query_embedding: &[f32],
    options: &SearchOptions,
) -> Result<Vec<SearchResult<ChunkRecord>>> {
    // Get candidate IDs from vector search
    // Request more than limit to account for filtering
    let candidate_limit = options.limit * 3;
    let candidates = search_similar(conn, CHUNK_VEC_TABLE, query_embedding, candidate_limit)?;

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Build filtered query
    let mut conditions = vec!["c.id IN (".to_string()];
    let placeholders: Vec<String> = candidates.iter().map(|_| "?".to_string()).collect();
    conditions.push(placeholders.join(","));
    conditions.push(")".to_string());

    if let Some(ref lang) = options.language {
        conditions.push(format!(" AND c.language = '{lang}'"));
    }

    if let Some(ref pattern) = options.path_pattern {
        conditions.push(format!(" AND c.file_path LIKE '{pattern}'"));
    }

    let sql = format!(
        "SELECT c.id, c.file_path, c.chunk_index, c.start_line, c.end_line, c.content, c.language, c.file_hash, c.indexed_at
         FROM chunks c
         WHERE {}",
        conditions.join("")
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::Database(format!("failed to prepare search: {e}")))?;

    // Create a map of id -> distance for quick lookup
    let distance_map: std::collections::HashMap<i64, f32> = candidates.iter().copied().collect();

    // Execute query with candidate IDs as parameters
    let params: Vec<i64> = candidates.iter().map(|(id, _)| *id).collect();
    let param_refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(ChunkRecord {
                id: Some(row.get(0)?),
                file_path: row.get(1)?,
                chunk_index: row.get(2)?,
                start_line: row.get(3)?,
                end_line: row.get(4)?,
                content: row.get(5)?,
                language: row.get(6)?,
                file_hash: row.get(7)?,
                indexed_at: row.get(8)?,
                embedding: None,
            })
        })
        .map_err(|e| StorageError::Database(format!("failed to execute search: {e}")))?;

    let mut results = Vec::new();
    for row in rows {
        let chunk =
            row.map_err(|e| StorageError::Database(format!("failed to read result: {e}")))?;
        let chunk_id = chunk.id.unwrap_or(0);
        let distance = distance_map.get(&chunk_id).copied().unwrap_or(f32::MAX);
        let result = SearchResult::new(chunk, distance);

        // Apply score filter
        if result.score >= options.min_score {
            results.push(result);
        }
    }

    // Sort by distance (ascending) and limit
    results.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(options.limit);

    tracing::debug!(
        count = results.len(),
        limit = options.limit,
        "Chunk search completed"
    );

    Ok(results)
}

/// Search for similar code by text (requires embedding generation).
///
/// This is a convenience wrapper that will be used when embeddings are available.
/// For now, it's a placeholder that returns an error.
///
/// # Errors
///
/// Returns an error because embeddings are not yet integrated.
pub fn search_chunks_by_text(
    _conn: &Connection,
    _query: &str,
    _options: &SearchOptions,
) -> Result<Vec<SearchResult<ChunkRecord>>> {
    Err(crate::Error::internal(
        "Text search requires embedding integration (Phase 1.3)",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_options_default() {
        let opts = SearchOptions::default();
        assert_eq!(opts.limit, 10);
        assert_eq!(opts.min_score, 0.0);
        assert!(opts.language.is_none());
        assert!(opts.path_pattern.is_none());
    }

    #[test]
    fn test_search_options_builder() {
        let opts = SearchOptions::new(20)
            .with_min_score(0.5)
            .with_language("rust")
            .with_path_pattern("%.rs");

        assert_eq!(opts.limit, 20);
        assert_eq!(opts.min_score, 0.5);
        assert_eq!(opts.language, Some("rust".to_string()));
        assert_eq!(opts.path_pattern, Some("%.rs".to_string()));
    }

    #[test]
    fn test_search_options_min_score_clamping() {
        let opts = SearchOptions::new(10).with_min_score(2.0);
        assert_eq!(opts.min_score, 1.0);

        let opts = SearchOptions::new(10).with_min_score(-0.5);
        assert_eq!(opts.min_score, 0.0);
    }

    // Integration tests that require sqlite-vec are in integration test files
}
