//! File state storage for incremental indexing.

use rusqlite::Connection;
use std::path::Path;

use super::models::FileState;
use crate::error::StorageError;
use crate::Result;

/// Get file state by path.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn get_file_state(conn: &Connection, path: &str) -> Result<Option<FileState>> {
    let result = conn.query_row(
        "SELECT path, mtime, size, hash, last_indexed FROM file_state WHERE path = ?",
        [path],
        |row| {
            Ok(FileState {
                path: row.get(0)?,
                mtime: row.get(1)?,
                size: row.get(2)?,
                hash: row.get(3)?,
                last_indexed: row.get(4)?,
            })
        },
    );

    match result {
        Ok(state) => Ok(Some(state)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(StorageError::Database(e.to_string()).into()),
    }
}

/// Update or insert file state.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn upsert_file_state(conn: &Connection, state: &FileState) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO file_state (path, mtime, size, hash, last_indexed) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![state.path, state.mtime, state.size, state.hash, state.last_indexed],
    )
    .map_err(|e| StorageError::Database(e.to_string()))?;
    Ok(())
}

/// Delete file state.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn delete_file_state(conn: &Connection, path: &str) -> Result<()> {
    conn.execute("DELETE FROM file_state WHERE path = ?", [path])
        .map_err(|e| StorageError::Database(e.to_string()))?;
    Ok(())
}

/// List all tracked file paths.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn list_file_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT path FROM file_state ORDER BY path")
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let paths = stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| StorageError::Database(e.to_string()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::Database(e.to_string()))?;

    Ok(paths)
}

/// Check if a file needs reindexing based on mtime.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn needs_reindex(conn: &Connection, path: &str, current_mtime: i64) -> Result<bool> {
    match get_file_state(conn, path)? {
        Some(state) => Ok(state.mtime < current_mtime),
        None => Ok(true), // New file
    }
}

/// Count tracked files.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn count_tracked_files(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM file_state", [], |row| row.get(0))
        .map_err(|e| StorageError::Database(e.to_string()).into())
}

/// Find stale entries (files no longer on disk).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn find_stale_entries(conn: &Connection, base_path: &Path) -> Result<Vec<String>> {
    let all_paths = list_file_paths(conn)?;
    let stale: Vec<String> = all_paths
        .into_iter()
        .filter(|p| {
            let path = Path::new(p);
            path.starts_with(base_path) && !path.exists()
        })
        .collect();
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{migrate, Database};

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| migrate(conn)).unwrap();
        db
    }

    #[test]
    fn test_upsert_and_get() {
        let db = setup_db();

        db.with_conn(|conn| {
            let state = FileState::new("/test/file.rs", 1234567890, 1024, "abc123");
            upsert_file_state(conn, &state)?;

            let retrieved = get_file_state(conn, "/test/file.rs")?.unwrap();
            assert_eq!(retrieved.path, "/test/file.rs");
            assert_eq!(retrieved.mtime, 1234567890);
            assert_eq!(retrieved.hash, "abc123");

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_get_nonexistent() {
        let db = setup_db();

        let result = db
            .with_conn(|conn| get_file_state(conn, "/nonexistent"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_delete() {
        let db = setup_db();

        db.with_conn(|conn| {
            upsert_file_state(conn, &FileState::new("/test.rs", 0, 0, "hash"))?;
            assert!(get_file_state(conn, "/test.rs")?.is_some());

            delete_file_state(conn, "/test.rs")?;
            assert!(get_file_state(conn, "/test.rs")?.is_none());

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_needs_reindex() {
        let db = setup_db();

        db.with_conn(|conn| {
            // New file needs reindex
            assert!(needs_reindex(conn, "/new.rs", 100)?);

            // Add file state
            upsert_file_state(conn, &FileState::new("/test.rs", 100, 0, "hash"))?;

            // Same mtime - no reindex
            assert!(!needs_reindex(conn, "/test.rs", 100)?);

            // Newer mtime - needs reindex
            assert!(needs_reindex(conn, "/test.rs", 200)?);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_list_and_count() {
        let db = setup_db();

        db.with_conn(|conn| {
            assert_eq!(count_tracked_files(conn)?, 0);

            upsert_file_state(conn, &FileState::new("/a.rs", 0, 0, "h"))?;
            upsert_file_state(conn, &FileState::new("/b.rs", 0, 0, "h"))?;

            assert_eq!(count_tracked_files(conn)?, 2);
            assert_eq!(list_file_paths(conn)?.len(), 2);

            Ok(())
        })
        .unwrap();
    }
}

/// Check if a file needs reindexing based on mtime AND size.
/// This is faster than hash comparison since it only needs stat() metadata,
/// not the full file content.
///
/// Returns `true` if the file needs reindexing:
/// - File is not in file_state (new file)
/// - mtime differs from stored value
/// - size differs from stored value
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn needs_reindex_by_metadata(
    conn: &Connection,
    path: &str,
    current_mtime: i64,
    current_size: i64,
) -> Result<bool> {
    match get_file_state(conn, path)? {
        Some(state) => Ok(state.mtime != current_mtime || state.size != current_size),
        None => Ok(true), // New file
    }
}

/// List tracked file paths under a given path prefix.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn list_file_paths_by_prefix(conn: &Connection, path_prefix: &str) -> Result<Vec<String>> {
    let prefix = if path_prefix.ends_with('/') {
        path_prefix.to_string()
    } else {
        format!("{path_prefix}/")
    };
    let pattern = format!("{prefix}%");

    let mut stmt = conn
        .prepare("SELECT path FROM file_state WHERE path LIKE ? ORDER BY path")
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let paths = stmt
        .query_map([&pattern], |row| row.get(0))
        .map_err(|e| StorageError::Database(e.to_string()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| StorageError::Database(e.to_string()))?;

    Ok(paths)
}

/// Delete file state entries under a path prefix.
///
/// Returns the number of entries deleted.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn delete_file_state_by_prefix(conn: &Connection, path_prefix: &str) -> Result<usize> {
    let prefix = if path_prefix.ends_with('/') {
        path_prefix.to_string()
    } else {
        format!("{path_prefix}/")
    };
    let pattern = format!("{prefix}%");

    let count = conn
        .execute("DELETE FROM file_state WHERE path LIKE ?", [&pattern])
        .map_err(|e| StorageError::Database(e.to_string()))?;

    Ok(count)
}
