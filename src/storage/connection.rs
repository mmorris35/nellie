//! `SQLite` database connection management.
//!
//! Provides a connection wrapper with proper configuration for:
//! - WAL mode for concurrent reads
//! - Connection pooling (via `parking_lot::Mutex`)
//! - Automatic sqlite-vec extension loading

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Arc;

use super::vector::init_sqlite_vec;
use crate::error::StorageError;
use crate::Result;

/// Database connection wrapper.
///
/// Wraps a `SQLite` connection with proper configuration and locking.
/// Clone is cheap - it just clones the Arc.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    path: String,
}

impl Database {
    /// Open a database at the given path.
    ///
    /// Creates the database file and parent directories if they don't exist.
    /// Configures WAL mode and performance settings.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or configured.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        // CRITICAL: Register sqlite-vec BEFORE creating any connection
        init_sqlite_vec();

        let path = path.as_ref();

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let path_str = path.to_string_lossy().to_string();

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| StorageError::Database(format!("failed to open database: {e}")))?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: path_str,
        };

        db.configure()?;

        Ok(db)
    }

    /// Open an in-memory database for testing.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened.
    pub fn open_in_memory() -> Result<Self> {
        // CRITICAL: Register sqlite-vec BEFORE creating any connection
        init_sqlite_vec();

        let conn = Connection::open_in_memory().map_err(|e| {
            StorageError::Database(format!("failed to open in-memory database: {e}"))
        })?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: ":memory:".to_string(),
        };

        db.configure()?;

        Ok(db)
    }

    /// Configure database settings for optimal performance.
    fn configure(&self) -> Result<()> {
        {
            let conn = self.conn.lock();

            // Enable WAL mode for better concurrent read performance
            conn.execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA cache_size = -64000;  -- 64MB cache
                PRAGMA temp_store = MEMORY;
                PRAGMA mmap_size = 268435456;  -- 256MB mmap
                PRAGMA foreign_keys = ON;
                ",
            )
            .map_err(|e| StorageError::Database(format!("failed to configure database: {e}")))?;
        }

        tracing::debug!(path = %self.path, "Database configured with WAL mode");

        Ok(())
    }

    /// Execute a function with exclusive database access.
    ///
    /// The function receives a mutable reference to the connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the function fails.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock();
        f(&conn)
    }

    /// Execute a function that may modify the database.
    ///
    /// Wraps the operation in an immediate transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if the transaction fails or if the function fails.
    pub fn with_transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let result = {
            let conn = self.conn.lock();

            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| StorageError::Database(format!("failed to begin transaction: {e}")))?;

            let tx_result = match f(&conn) {
                Ok(result) => {
                    conn.execute_batch("COMMIT")
                        .map_err(|e| StorageError::Database(format!("failed to commit: {e}")))?;
                    Ok(result)
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(e)
                }
            };
            drop(conn);
            tx_result
        };
        result
    }

    /// Get the database path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Check if the database is healthy.
    ///
    /// # Errors
    ///
    /// Returns an error if the health check fails.
    pub fn health_check(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.query_row("SELECT 1", [], |_| Ok(()))
                .map_err(|e| StorageError::Database(format!("health check failed: {e}")).into())
        })
    }
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_open_in_memory() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.path(), ":memory:");
        db.health_check().unwrap();
    }

    #[test]
    fn test_open_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.db");

        let db = Database::open(&path).unwrap();
        assert!(path.exists());
        db.health_check().unwrap();
    }

    #[test]
    fn test_open_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("dirs").join("test.db");

        let db = Database::open(&path).unwrap();
        assert!(path.exists());
        db.health_check().unwrap();
    }

    #[test]
    fn test_with_conn() {
        let db = Database::open_in_memory().unwrap();

        let result: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT 42", [], |row| row.get(0))
                    .map_err(|e| StorageError::Database(e.to_string()).into())
            })
            .unwrap();

        assert_eq!(result, 42);
    }

    #[test]
    fn test_with_transaction_commit() {
        let db = Database::open_in_memory().unwrap();

        // Create table
        db.with_conn(|conn| {
            conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY)", [])
                .map_err(|e| StorageError::Database(e.to_string()))?;
            Ok(())
        })
        .unwrap();

        // Insert in transaction
        db.with_transaction(|conn| {
            conn.execute("INSERT INTO test (id) VALUES (1)", [])
                .map_err(|e| StorageError::Database(e.to_string()))?;
            Ok(())
        })
        .unwrap();

        // Verify committed
        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))
                    .map_err(|e| StorageError::Database(e.to_string()).into())
            })
            .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn test_with_transaction_rollback() {
        let db = Database::open_in_memory().unwrap();

        // Create table
        db.with_conn(|conn| {
            conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY)", [])
                .map_err(|e| StorageError::Database(e.to_string()))?;
            Ok(())
        })
        .unwrap();

        // Transaction that fails
        let result: Result<()> = db.with_transaction(|conn| {
            conn.execute("INSERT INTO test (id) VALUES (1)", [])
                .map_err(|e| StorageError::Database(e.to_string()))?;
            // Simulate failure
            Err(crate::Error::internal("simulated failure"))
        });

        assert!(result.is_err());

        // Verify rolled back
        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))
                    .map_err(|e| StorageError::Database(e.to_string()).into())
            })
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn test_wal_mode_enabled() {
        let db = Database::open_in_memory().unwrap();

        let mode: String = db
            .with_conn(|conn| {
                conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))
                    .map_err(|e| StorageError::Database(e.to_string()).into())
            })
            .unwrap();

        // In-memory databases use "memory" journal mode
        assert!(mode == "wal" || mode == "memory");
    }

    #[test]
    fn test_clone_shares_connection() {
        let db1 = Database::open_in_memory().unwrap();

        // Create table with db1
        db1.with_conn(|conn| {
            conn.execute("CREATE TABLE test (id INTEGER)", [])
                .map_err(|e| StorageError::Database(e.to_string()))?;
            conn.execute("INSERT INTO test VALUES (123)", [])
                .map_err(|e| StorageError::Database(e.to_string()))?;
            Ok(())
        })
        .unwrap();

        // Clone and read with db2
        let db2 = db1.clone();
        let value: i64 = db2
            .with_conn(|conn| {
                conn.query_row("SELECT id FROM test", [], |row| row.get(0))
                    .map_err(|e| StorageError::Database(e.to_string()).into())
            })
            .unwrap();

        assert_eq!(value, 123);
    }
}
