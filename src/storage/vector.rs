//! Vector search support using sqlite-vec extension.
//!
//! Provides helpers for creating and querying vec0 virtual tables
//! for efficient similarity search.

use rusqlite::Connection;
use sqlite_vec::sqlite3_vec_init;
use std::sync::Once;

use crate::error::StorageError;
use crate::Result;

/// Vector dimension for embeddings.
/// all-MiniLM-L6-v2 produces 384-dimensional vectors.
pub const EMBEDDING_DIM: usize = 384;

// Static guard to ensure sqlite-vec is initialized exactly once
static INIT: Once = Once::new();

/// Initialize sqlite-vec extension globally.
///
/// This must be called before any database connections are created.
/// Uses `sqlite3_auto_extension` to register the extension globally
/// so it's automatically available in all new connections.
///
/// # Safety
///
/// This function is safe to call multiple times - the `Once` guard ensures
/// initialization happens exactly once. Subsequent calls are no-ops.
#[allow(unsafe_code)]
pub fn init_sqlite_vec() {
    INIT.call_once(|| {
        // SAFETY: This is safe because:
        // 1. `sqlite3_vec_init` is a valid function pointer from sqlite-vec
        // 2. `sqlite3_auto_extension` expects a valid extension initializer function
        // 3. We transmute the function pointer to the expected signature (int (*)(sqlite3*, char**, const sqlite3_api_routines*))
        // 4. The Once guard ensures this is called exactly once, preventing double-initialization
        // 5. This follows the standard sqlite extension loading pattern described in https://alexgarcia.xyz/sqlite-vec/rust.html
        #[allow(clippy::missing_transmute_annotations)]
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite3_vec_init as *const (),
            )));
        }
        tracing::info!("sqlite-vec extension registered globally via sqlite3_auto_extension");
    });
}

/// Load sqlite-vec extension into a connection.
///
/// This verifies that the sqlite-vec extension is available and working.
/// The actual registration happens in `init_sqlite_vec()` which must be
/// called before any database connections are opened.
///
/// # Errors
///
/// Returns an error if the extension cannot be loaded or verified.
pub fn load_extension(conn: &Connection) -> Result<()> {
    // Verify sqlite-vec is available by calling vec_version()
    match conn.execute_batch("SELECT vec_version();") {
        Ok(()) => {
            tracing::debug!("sqlite-vec extension verified");
            Ok(())
        }
        Err(e) => {
            let err_msg = format!(
                "sqlite-vec extension not available. \
                 Vector search will not work. \
                 This is a CRITICAL error - embeddings cannot be stored. \
                 Make sure init_sqlite_vec() was called before database init. \
                 Error: {e}"
            );
            tracing::error!("{err_msg}");
            Err(crate::error::StorageError::Vector(err_msg).into())
        }
    }
}

/// Create a vec0 virtual table for vector similarity search.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `table_name` - Name for the virtual table
/// * `dimension` - Vector dimension (e.g., 384 for all-MiniLM-L6-v2)
///
/// # Errors
///
/// Returns an error if the table cannot be created.
pub fn create_vec_table(conn: &Connection, table_name: &str, dimension: usize) -> Result<()> {
    let sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {table_name} USING vec0(
            id INTEGER PRIMARY KEY,
            embedding FLOAT[{dimension}]
        )"
    );

    conn.execute(&sql, [])
        .map_err(|e| StorageError::Vector(format!("failed to create vec table: {e}")))?;

    tracing::debug!(table = table_name, dim = dimension, "Created vec0 table");
    Ok(())
}

/// Insert a vector into a vec0 table.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `table_name` - Name of the vec0 virtual table
/// * `id` - Row ID to associate with this vector
/// * `embedding` - The embedding vector (must match table dimension)
///
/// # Errors
///
/// Returns an error if the insertion fails.
pub fn insert_vector(
    conn: &Connection,
    table_name: &str,
    id: i64,
    embedding: &[f32],
) -> Result<()> {
    let blob = vector_to_blob(embedding);

    let sql = format!("INSERT INTO {table_name} (id, embedding) VALUES (?, ?)");
    conn.execute(&sql, rusqlite::params![id, blob])
        .map_err(|e| StorageError::Vector(format!("failed to insert vector: {e}")))?;

    Ok(())
}

/// Search for similar vectors using cosine distance.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `table_name` - Name of the vec0 virtual table
/// * `query_embedding` - The query vector to find similar vectors to
/// * `limit` - Maximum number of results to return
///
/// # Returns
///
/// Vector of (id, distance) pairs, sorted by distance ascending (most similar first).
///
/// # Errors
///
/// Returns an error if the search fails.
pub fn search_similar(
    conn: &Connection,
    table_name: &str,
    query_embedding: &[f32],
    limit: usize,
) -> Result<Vec<(i64, f32)>> {
    let blob = vector_to_blob(query_embedding);

    let sql = format!(
        "SELECT id, distance
         FROM {table_name}
         WHERE embedding MATCH ?
         ORDER BY distance
         LIMIT ?"
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::Vector(format!("failed to prepare search: {e}")))?;

    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let results = stmt
        .query_map(rusqlite::params![blob, limit_i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f32>(1)?))
        })
        .map_err(|e| StorageError::Vector(format!("failed to execute search: {e}")))?;

    let mut matches = Vec::new();
    for result in results {
        let (id, distance) =
            result.map_err(|e| StorageError::Vector(format!("failed to read result: {e}")))?;
        matches.push((id, distance));
    }

    Ok(matches)
}

/// Delete a vector from a vec0 table.
///
/// # Errors
///
/// Returns an error if the deletion fails.
pub fn delete_vector(conn: &Connection, table_name: &str, id: i64) -> Result<()> {
    let sql = format!("DELETE FROM {table_name} WHERE id = ?");
    conn.execute(&sql, rusqlite::params![id])
        .map_err(|e| StorageError::Vector(format!("failed to delete vector: {e}")))?;
    Ok(())
}

/// Convert a vector to a blob for storage.
fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Convert a blob back to a vector.
#[allow(dead_code)]
fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Database;

    fn create_test_db() -> Database {
        // Initialize sqlite-vec globally before creating database
        init_sqlite_vec();

        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            load_extension(conn)?;
            Ok(())
        })
        .unwrap();
        db
    }

    #[test]
    fn test_init_sqlite_vec() {
        // Calling multiple times should be safe (Once guard)
        init_sqlite_vec();
        init_sqlite_vec();
        // If we get here without panicking, the test passes
    }

    #[test]
    fn test_load_extension() {
        init_sqlite_vec();
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            let result = load_extension(conn);
            // Should succeed now that init_sqlite_vec has been called
            assert!(
                result.is_ok(),
                "sqlite-vec should be available after init_sqlite_vec: {:?}",
                result
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_vector_blob_roundtrip() {
        let original = vec![1.0f32, 2.0, 3.0, -4.5];
        let blob = vector_to_blob(&original);
        let recovered = blob_to_vector(&blob);
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_vector_blob_empty() {
        let original: Vec<f32> = vec![];
        let blob = vector_to_blob(&original);
        let recovered = blob_to_vector(&blob);
        assert!(recovered.is_empty());
    }

    // Integration tests that require sqlite-vec extension
    // These should now pass without #[ignore] since init_sqlite_vec is called
    #[test]
    fn test_create_vec_table() {
        let db = create_test_db();
        db.with_conn(|conn| {
            create_vec_table(conn, "test_vectors", 4)?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_insert_and_search() {
        let db = create_test_db();

        db.with_conn(|conn| {
            create_vec_table(conn, "test_vectors", 4)?;

            // Insert test vectors
            insert_vector(conn, "test_vectors", 1, &[1.0, 0.0, 0.0, 0.0])?;
            insert_vector(conn, "test_vectors", 2, &[0.9, 0.1, 0.0, 0.0])?;
            insert_vector(conn, "test_vectors", 3, &[0.0, 1.0, 0.0, 0.0])?;

            // Search for similar to [1, 0, 0, 0]
            let results = search_similar(conn, "test_vectors", &[1.0, 0.0, 0.0, 0.0], 3)?;

            assert_eq!(results.len(), 3);
            // First result should be exact match (id=1)
            assert_eq!(results[0].0, 1);
            // Second should be id=2 (most similar)
            assert_eq!(results[1].0, 2);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_delete_vector() {
        let db = create_test_db();

        db.with_conn(|conn| {
            create_vec_table(conn, "test_vectors", 4)?;
            insert_vector(conn, "test_vectors", 1, &[1.0, 0.0, 0.0, 0.0])?;
            insert_vector(conn, "test_vectors", 2, &[0.0, 1.0, 0.0, 0.0])?;

            delete_vector(conn, "test_vectors", 1)?;

            let results = search_similar(conn, "test_vectors", &[1.0, 0.0, 0.0, 0.0], 10)?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].0, 2);

            Ok(())
        })
        .unwrap();
    }
}
