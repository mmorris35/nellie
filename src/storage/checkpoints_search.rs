//! Checkpoint semantic search.

use rusqlite::Connection;

use super::models::{CheckpointRecord, SearchResult};
use crate::error::StorageError;
use crate::Result;

const CHECKPOINT_VEC_TABLE: &str = "checkpoint_embeddings";

/// Initialize checkpoint vector table.
///
/// # Errors
///
/// Returns an error if the table cannot be created.
pub fn init_checkpoint_vectors(conn: &Connection) -> Result<()> {
    let sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {CHECKPOINT_VEC_TABLE} USING vec0(
            id TEXT PRIMARY KEY,
            embedding FLOAT[384]
        )"
    );

    conn.execute(&sql, [])
        .map_err(|e| StorageError::Vector(format!("failed to create checkpoint vec table: {e}")))?;

    Ok(())
}

/// Store checkpoint embedding.
///
/// # Errors
///
/// Returns an error if the embedding cannot be stored.
pub fn store_checkpoint_embedding(
    conn: &Connection,
    checkpoint_id: &str,
    embedding: &[f32],
) -> Result<()> {
    // Delete old embedding if exists
    conn.execute(
        &format!("DELETE FROM {CHECKPOINT_VEC_TABLE} WHERE id = ?"),
        [checkpoint_id],
    )
    .ok();

    // Insert new embedding
    let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    conn.execute(
        &format!("INSERT INTO {CHECKPOINT_VEC_TABLE} (id, embedding) VALUES (?, ?)"),
        rusqlite::params![checkpoint_id, blob],
    )
    .map_err(|e| StorageError::Vector(format!("failed to store checkpoint embedding: {e}")))?;

    Ok(())
}

/// Search checkpoints by embedding similarity.
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_checkpoints_by_embedding(
    conn: &Connection,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<SearchResult<CheckpointRecord>>> {
    let blob: Vec<u8> = query_embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();

    let sql = format!(
        "SELECT id, distance FROM {CHECKPOINT_VEC_TABLE} WHERE embedding MATCH ? ORDER BY distance LIMIT ?"
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
        if let Ok(checkpoint) = super::checkpoints::get_checkpoint(conn, &id) {
            results.push(SearchResult::new(checkpoint, distance));
        }
    }

    Ok(results)
}

/// Search checkpoints by text match (LIKE on `working_on` field).
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_checkpoints_by_text(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<CheckpointRecord>> {
    let pattern = format!("%{query}%");

    let mut stmt = conn
        .prepare(
            "SELECT id, agent, repo, session_id, working_on, state, created_at
             FROM checkpoints
             WHERE working_on LIKE ?
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let checkpoints = stmt
        .query_map(
            rusqlite::params![&pattern, i64::try_from(limit).unwrap_or(10)],
            |row| {
                let state_json: String = row.get(5)?;
                let state: serde_json::Value =
                    serde_json::from_str(&state_json).unwrap_or_default();

                Ok(CheckpointRecord {
                    id: row.get(0)?,
                    agent: row.get(1)?,
                    repo: row.get(2)?,
                    session_id: row.get(3)?,
                    working_on: row.get(4)?,
                    state,
                    created_at: row.get(6)?,
                })
            },
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for checkpoint in checkpoints {
        result.push(checkpoint.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Search checkpoints by agent name.
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_checkpoints_by_agent(
    conn: &Connection,
    agent: &str,
    limit: usize,
) -> Result<Vec<CheckpointRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent, repo, session_id, working_on, state, created_at
             FROM checkpoints
             WHERE agent = ?
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let checkpoints = stmt
        .query_map(
            rusqlite::params![agent, i64::try_from(limit).unwrap_or(10)],
            |row| {
                let state_json: String = row.get(5)?;
                let state: serde_json::Value =
                    serde_json::from_str(&state_json).unwrap_or_default();

                Ok(CheckpointRecord {
                    id: row.get(0)?,
                    agent: row.get(1)?,
                    repo: row.get(2)?,
                    session_id: row.get(3)?,
                    working_on: row.get(4)?,
                    state,
                    created_at: row.get(6)?,
                })
            },
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for checkpoint in checkpoints {
        result.push(checkpoint.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Search checkpoints by repository.
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_checkpoints_by_repo(
    conn: &Connection,
    repo: &str,
    limit: usize,
) -> Result<Vec<CheckpointRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent, repo, session_id, working_on, state, created_at
             FROM checkpoints
             WHERE repo = ?
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let checkpoints = stmt
        .query_map(
            rusqlite::params![repo, i64::try_from(limit).unwrap_or(10)],
            |row| {
                let state_json: String = row.get(5)?;
                let state: serde_json::Value =
                    serde_json::from_str(&state_json).unwrap_or_default();

                Ok(CheckpointRecord {
                    id: row.get(0)?,
                    agent: row.get(1)?,
                    repo: row.get(2)?,
                    session_id: row.get(3)?,
                    working_on: row.get(4)?,
                    state,
                    created_at: row.get(6)?,
                })
            },
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for checkpoint in checkpoints {
        result.push(checkpoint.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Search checkpoints by session ID.
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_checkpoints_by_session(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> Result<Vec<CheckpointRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent, repo, session_id, working_on, state, created_at
             FROM checkpoints
             WHERE session_id = ?
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let checkpoints = stmt
        .query_map(
            rusqlite::params![session_id, i64::try_from(limit).unwrap_or(10)],
            |row| {
                let state_json: String = row.get(5)?;
                let state: serde_json::Value =
                    serde_json::from_str(&state_json).unwrap_or_default();

                Ok(CheckpointRecord {
                    id: row.get(0)?,
                    agent: row.get(1)?,
                    repo: row.get(2)?,
                    session_id: row.get(3)?,
                    working_on: row.get(4)?,
                    state,
                    created_at: row.get(6)?,
                })
            },
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for checkpoint in checkpoints {
        result.push(checkpoint.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

/// Search checkpoints by multiple criteria (agent AND repo).
///
/// # Errors
///
/// Returns an error if the search query fails.
pub fn search_checkpoints_by_agent_and_repo(
    conn: &Connection,
    agent: &str,
    repo: &str,
    limit: usize,
) -> Result<Vec<CheckpointRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, agent, repo, session_id, working_on, state, created_at
             FROM checkpoints
             WHERE agent = ? AND repo = ?
             ORDER BY created_at DESC
             LIMIT ?",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let checkpoints = stmt
        .query_map(
            rusqlite::params![agent, repo, i64::try_from(limit).unwrap_or(10)],
            |row| {
                let state_json: String = row.get(5)?;
                let state: serde_json::Value =
                    serde_json::from_str(&state_json).unwrap_or_default();

                Ok(CheckpointRecord {
                    id: row.get(0)?,
                    agent: row.get(1)?,
                    repo: row.get(2)?,
                    session_id: row.get(3)?,
                    working_on: row.get(4)?,
                    state,
                    created_at: row.get(6)?,
                })
            },
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for checkpoint in checkpoints {
        result.push(checkpoint.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{insert_checkpoint, migrate, Database};

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| migrate(conn)).unwrap();
        db
    }

    #[test]
    fn test_search_by_text() {
        let db = setup_db();

        db.with_conn(|conn| {
            insert_checkpoint(
                conn,
                &CheckpointRecord::new(
                    "claude-1",
                    "Working on feature implementation",
                    serde_json::json!({}),
                ),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new(
                    "claude-2",
                    "Debugging test failures",
                    serde_json::json!({}),
                ),
            )?;

            let results = search_checkpoints_by_text(conn, "feature", 10)?;
            assert_eq!(results.len(), 1);
            assert!(results[0].working_on.contains("feature"));

            let results = search_checkpoints_by_text(conn, "Debugging", 10)?;
            assert_eq!(results.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_agent() {
        let db = setup_db();

        db.with_conn(|conn| {
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-1", "Task A", serde_json::json!({})),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-1", "Task B", serde_json::json!({})),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-2", "Task C", serde_json::json!({})),
            )?;

            let results = search_checkpoints_by_agent(conn, "agent-1", 10)?;
            assert_eq!(results.len(), 2);

            let results = search_checkpoints_by_agent(conn, "agent-2", 10)?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].agent, "agent-2");

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_repo() {
        let db = setup_db();

        db.with_conn(|conn| {
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-1", "Task 1", serde_json::json!({}))
                    .with_repo("repo-1"),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-2", "Task 2", serde_json::json!({}))
                    .with_repo("repo-1"),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-3", "Task 3", serde_json::json!({}))
                    .with_repo("repo-2"),
            )?;

            let results = search_checkpoints_by_repo(conn, "repo-1", 10)?;
            assert_eq!(results.len(), 2);

            let results = search_checkpoints_by_repo(conn, "repo-2", 10)?;
            assert_eq!(results.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_session() {
        let db = setup_db();

        db.with_conn(|conn| {
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-1", "Task A", serde_json::json!({}))
                    .with_session("session-123"),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-1", "Task B", serde_json::json!({}))
                    .with_session("session-123"),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-2", "Task C", serde_json::json!({}))
                    .with_session("session-456"),
            )?;

            let results = search_checkpoints_by_session(conn, "session-123", 10)?;
            assert_eq!(results.len(), 2);

            let results = search_checkpoints_by_session(conn, "session-456", 10)?;
            assert_eq!(results.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_search_by_agent_and_repo() {
        let db = setup_db();

        db.with_conn(|conn| {
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-1", "Task 1", serde_json::json!({}))
                    .with_repo("repo-1"),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-1", "Task 2", serde_json::json!({}))
                    .with_repo("repo-2"),
            )?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("agent-2", "Task 3", serde_json::json!({}))
                    .with_repo("repo-1"),
            )?;

            let results = search_checkpoints_by_agent_and_repo(conn, "agent-1", "repo-1", 10)?;
            assert_eq!(results.len(), 1);

            let results = search_checkpoints_by_agent_and_repo(conn, "agent-2", "repo-1", 10)?;
            assert_eq!(results.len(), 1);

            let results = search_checkpoints_by_agent_and_repo(conn, "agent-1", "repo-2", 10)?;
            assert_eq!(results.len(), 1);

            let results = search_checkpoints_by_agent_and_repo(conn, "nonexistent", "repo-1", 10)?;
            assert_eq!(results.len(), 0);

            Ok(())
        })
        .unwrap();
    }
}
