//! Chunk storage operations.
//!
//! Provides CRUD operations for code chunks with their embeddings.

use rusqlite::{params, Connection};

use super::models::ChunkRecord;
use super::vector::{delete_vector, insert_vector, EMBEDDING_DIM};
use crate::error::StorageError;
use crate::Result;

/// Vector table name for chunk embeddings.
const CHUNK_VEC_TABLE: &str = "chunk_embeddings";

/// Initialize chunk vector table.
///
/// # Errors
///
/// Returns an error if the table cannot be created.
pub fn init_chunk_vectors(conn: &Connection) -> Result<()> {
    // Create vec0 table for chunk embeddings
    let sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {CHUNK_VEC_TABLE} USING vec0(
            id INTEGER PRIMARY KEY,
            embedding FLOAT[{EMBEDDING_DIM}]
        )"
    );

    conn.execute(&sql, [])
        .map_err(|e| StorageError::Vector(format!("failed to create chunk vec table: {e}")))?;

    tracing::debug!("Chunk vector table initialized");
    Ok(())
}

/// Insert a chunk into the database.
///
/// Returns the assigned ID.
///
/// # Errors
///
/// Returns an error if the insertion fails.
pub fn insert_chunk(conn: &Connection, chunk: &ChunkRecord) -> Result<i64> {
    let sql = "
        INSERT INTO chunks (file_path, chunk_index, start_line, end_line, content, language, file_hash, indexed_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    ";

    conn.execute(
        sql,
        params![
            chunk.file_path,
            chunk.chunk_index,
            chunk.start_line,
            chunk.end_line,
            chunk.content,
            chunk.language,
            chunk.file_hash,
            chunk.indexed_at,
        ],
    )
    .map_err(|e| StorageError::Database(format!("failed to insert chunk: {e}")))?;

    let id = conn.last_insert_rowid();

    // Insert embedding if available
    if let Some(ref embedding) = chunk.embedding {
        insert_vector(conn, CHUNK_VEC_TABLE, id, embedding)?;
    }

    tracing::trace!(id, path = %chunk.file_path, "Inserted chunk");
    Ok(id)
}

/// Insert multiple chunks in a batch.
///
/// Returns the IDs of inserted chunks.
///
/// # Errors
///
/// Returns an error if any insertion fails.
pub fn insert_chunks_batch(conn: &Connection, chunks: &[ChunkRecord]) -> Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        let id = insert_chunk(conn, chunk)?;
        ids.push(id);
    }

    tracing::debug!(count = chunks.len(), "Inserted chunk batch");
    Ok(ids)
}

/// Get a chunk by ID.
///
/// # Errors
///
/// Returns an error if the chunk is not found or query fails.
pub fn get_chunk(conn: &Connection, id: i64) -> Result<ChunkRecord> {
    let sql = "
        SELECT id, file_path, chunk_index, start_line, end_line, content, language, file_hash, indexed_at
        FROM chunks
        WHERE id = ?
    ";

    conn.query_row(sql, [id], |row| {
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
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound {
            entity: "chunk",
            id: id.to_string(),
        }
        .into(),
        e => StorageError::Database(format!("failed to get chunk: {e}")).into(),
    })
}

/// Get all chunks for a file.
///
/// # Errors
///
/// Returns an error if the query fails.
pub fn get_chunks_by_file(conn: &Connection, file_path: &str) -> Result<Vec<ChunkRecord>> {
    let sql = "
        SELECT id, file_path, chunk_index, start_line, end_line, content, language, file_hash, indexed_at
        FROM chunks
        WHERE file_path = ?
        ORDER BY chunk_index
    ";

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let chunks = stmt
        .query_map([file_path], |row| {
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
        .map_err(|e| StorageError::Database(format!("failed to query chunks: {e}")))?;

    let mut result = Vec::new();
    for chunk in chunks {
        result
            .push(chunk.map_err(|e| StorageError::Database(format!("failed to read chunk: {e}")))?);
    }

    Ok(result)
}

/// Delete a chunk by ID.
///
/// # Errors
///
/// Returns an error if the deletion fails.
pub fn delete_chunk(conn: &Connection, id: i64) -> Result<()> {
    // Delete from vector table first
    let _ = delete_vector(conn, CHUNK_VEC_TABLE, id);

    // Delete from chunks table
    conn.execute("DELETE FROM chunks WHERE id = ?", [id])
        .map_err(|e| StorageError::Database(format!("failed to delete chunk: {e}")))?;

    tracing::trace!(id, "Deleted chunk");
    Ok(())
}

/// Delete all chunks for a file.
///
/// Returns the number of chunks deleted.
///
/// # Errors
///
/// Returns an error if the deletion fails.
pub fn delete_chunks_by_file(conn: &Connection, file_path: &str) -> Result<usize> {
    // Get chunk IDs first for vector deletion
    let ids: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM chunks WHERE file_path = ?")
            .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

        let mapped_rows = stmt
            .query_map([file_path], |row| row.get(0))
            .map_err(|e| StorageError::Database(format!("failed to query: {e}")))?;

        mapped_rows.flatten().collect()
    };

    // Delete from vector table
    for id in &ids {
        let _ = delete_vector(conn, CHUNK_VEC_TABLE, *id);
    }

    // Delete from chunks table
    let count = conn
        .execute("DELETE FROM chunks WHERE file_path = ?", [file_path])
        .map_err(|e| StorageError::Database(format!("failed to delete chunks: {e}")))?;

    tracing::debug!(path = file_path, count, "Deleted chunks for file");
    Ok(count)
}

/// Update a chunk's embedding.
///
/// # Errors
///
/// Returns an error if the update fails.
pub fn update_chunk_embedding(conn: &Connection, id: i64, embedding: &[f32]) -> Result<()> {
    // Delete old embedding if exists
    let _ = delete_vector(conn, CHUNK_VEC_TABLE, id);

    // Insert new embedding
    insert_vector(conn, CHUNK_VEC_TABLE, id, embedding)?;

    tracing::trace!(id, "Updated chunk embedding");
    Ok(())
}

/// Count total chunks in database.
///
/// # Errors
///
/// Returns an error if the query fails.
pub fn count_chunks(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
        .map_err(|e| StorageError::Database(format!("failed to count chunks: {e}")).into())
}

/// Count chunks for a specific file.
///
/// # Errors
///
/// Returns an error if the query fails.
pub fn count_chunks_for_file(conn: &Connection, file_path: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE file_path = ?",
        [file_path],
        |row| row.get(0),
    )
    .map_err(|e| StorageError::Database(format!("failed to count chunks: {e}")).into())
}

/// Delete all chunks under a path prefix (directory).
///
/// Returns the number of chunks deleted.
///
/// # Errors
///
/// Returns an error if the deletion fails.
pub fn delete_chunks_by_path_prefix(conn: &Connection, path_prefix: &str) -> Result<usize> {
    // Ensure the prefix ends with a separator for proper directory matching
    let prefix = if path_prefix.ends_with('/') {
        path_prefix.to_string()
    } else {
        format!("{path_prefix}/")
    };

    // Get chunk IDs first for vector deletion
    let ids: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM chunks WHERE file_path LIKE ?")
            .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

        let pattern = format!("{prefix}%");
        let mapped_rows = stmt
            .query_map([&pattern], |row| row.get(0))
            .map_err(|e| StorageError::Database(format!("failed to query: {e}")))?;

        mapped_rows.flatten().collect()
    };

    // Delete from vector table
    for id in &ids {
        let _ = delete_vector(conn, CHUNK_VEC_TABLE, *id);
    }

    // Delete from chunks table
    let pattern = format!("{prefix}%");
    let count = conn
        .execute("DELETE FROM chunks WHERE file_path LIKE ?", [&pattern])
        .map_err(|e| StorageError::Database(format!("failed to delete chunks: {e}")))?;

    tracing::debug!(path_prefix, count, "Deleted chunks under path prefix");
    Ok(count)
}

/// List all unique file paths under a path prefix.
///
/// # Errors
///
/// Returns an error if the query fails.
pub fn list_files_by_path_prefix(conn: &Connection, path_prefix: &str) -> Result<Vec<String>> {
    let prefix = if path_prefix.ends_with('/') {
        path_prefix.to_string()
    } else {
        format!("{path_prefix}/")
    };
    let pattern = format!("{prefix}%");

    let mut stmt = conn
        .prepare("SELECT DISTINCT file_path FROM chunks WHERE file_path LIKE ? ORDER BY file_path")
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let paths = stmt
        .query_map([&pattern], |row| row.get(0))
        .map_err(|e| StorageError::Database(format!("failed to query: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::Database(format!("failed to collect paths: {e}")))?;

    Ok(paths)
}

/// Count chunks under a path prefix (directory).
///
/// # Errors
///
/// Returns an error if the query fails.
pub fn count_chunks_by_path_prefix(conn: &Connection, path_prefix: &str) -> Result<i64> {
    let prefix = if path_prefix.ends_with('/') {
        path_prefix.to_string()
    } else {
        format!("{path_prefix}/")
    };
    let pattern = format!("{prefix}%");

    conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE file_path LIKE ?",
        [&pattern],
        |row| row.get(0),
    )
    .map_err(|e| StorageError::Database(format!("failed to count chunks: {e}")).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{migrate, Database};

    fn setup_test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            // Skip vector table creation for unit tests
            Ok(())
        })
        .unwrap();
        db
    }

    #[test]
    fn test_insert_and_get_chunk() {
        let db = setup_test_db();

        db.with_conn(|conn| {
            let chunk = ChunkRecord::new("/test/file.rs", 0, 1, 10, "fn main() {}", "hash123")
                .with_language("rust");

            let id = insert_chunk(conn, &chunk)?;
            assert!(id > 0);

            let retrieved = get_chunk(conn, id)?;
            assert_eq!(retrieved.file_path, "/test/file.rs");
            assert_eq!(retrieved.chunk_index, 0);
            assert_eq!(retrieved.content, "fn main() {}");
            assert_eq!(retrieved.language, Some("rust".to_string()));

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_get_chunk_not_found() {
        let db = setup_test_db();

        let result = db.with_conn(|conn| get_chunk(conn, 99999));

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_insert_batch() {
        let db = setup_test_db();

        db.with_conn(|conn| {
            let chunks: Vec<ChunkRecord> = (0..5)
                .map(|i| {
                    ChunkRecord::new(
                        "/test/file.rs",
                        i,
                        i + 1,
                        i + 10,
                        format!("content {i}"),
                        "hash",
                    )
                })
                .collect();

            let ids = insert_chunks_batch(conn, &chunks)?;
            assert_eq!(ids.len(), 5);

            let count = count_chunks_for_file(conn, "/test/file.rs")?;
            assert_eq!(count, 5);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_get_chunks_by_file() {
        let db = setup_test_db();

        db.with_conn(|conn| {
            // Insert chunks for two files
            insert_chunk(
                conn,
                &ChunkRecord::new("/file1.rs", 0, 1, 5, "content1", "hash1"),
            )?;
            insert_chunk(
                conn,
                &ChunkRecord::new("/file1.rs", 1, 6, 10, "content2", "hash1"),
            )?;
            insert_chunk(
                conn,
                &ChunkRecord::new("/file2.rs", 0, 1, 5, "content3", "hash2"),
            )?;

            let chunks = get_chunks_by_file(conn, "/file1.rs")?;
            assert_eq!(chunks.len(), 2);
            assert_eq!(chunks[0].chunk_index, 0);
            assert_eq!(chunks[1].chunk_index, 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_delete_chunk() {
        let db = setup_test_db();

        db.with_conn(|conn| {
            let chunk = ChunkRecord::new("/test/file.rs", 0, 1, 10, "content", "hash");
            let id = insert_chunk(conn, &chunk)?;

            delete_chunk(conn, id)?;

            let result = get_chunk(conn, id);
            assert!(result.is_err());

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_delete_chunks_by_file() {
        let db = setup_test_db();

        db.with_conn(|conn| {
            insert_chunk(conn, &ChunkRecord::new("/file1.rs", 0, 1, 5, "c1", "h1"))?;
            insert_chunk(conn, &ChunkRecord::new("/file1.rs", 1, 6, 10, "c2", "h1"))?;
            insert_chunk(conn, &ChunkRecord::new("/file2.rs", 0, 1, 5, "c3", "h2"))?;

            let deleted = delete_chunks_by_file(conn, "/file1.rs")?;
            assert_eq!(deleted, 2);

            let remaining = count_chunks(conn)?;
            assert_eq!(remaining, 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_count_chunks() {
        let db = setup_test_db();

        db.with_conn(|conn| {
            assert_eq!(count_chunks(conn)?, 0);

            insert_chunk(conn, &ChunkRecord::new("/f1.rs", 0, 1, 5, "c", "h"))?;
            insert_chunk(conn, &ChunkRecord::new("/f2.rs", 0, 1, 5, "c", "h"))?;

            assert_eq!(count_chunks(conn)?, 2);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_unique_constraint() {
        let db = setup_test_db();

        let result = db.with_conn(|conn| {
            insert_chunk(conn, &ChunkRecord::new("/file.rs", 0, 1, 5, "c1", "h1"))?;
            // Same file_path + chunk_index should fail
            insert_chunk(conn, &ChunkRecord::new("/file.rs", 0, 1, 5, "c2", "h2"))
        });

        assert!(result.is_err());
    }
}
