//! SQLite persistence for extracted symbols and structural edges.
//!
//! Provides CRUD operations for the `symbols` and `structural_edges` tables.

use rusqlite::Connection;

use crate::error::StorageError;
use crate::Result;

use super::extractor::{ExtractedSymbol, SymbolKind};

/// A symbol record as stored in the database.
#[derive(Debug, Clone)]
pub struct SymbolRecord {
    pub id: i64,
    pub file_path: String,
    pub symbol_name: String,
    pub symbol_kind: SymbolKind,
    pub language: String,
    pub start_line: i32,
    pub end_line: i32,
    pub scope: Option<String>,
    pub signature: Option<String>,
    pub file_hash: String,
    pub indexed_at: i64,
}

/// Store symbols for a file, replacing any existing symbols for that file.
///
/// # Errors
///
/// Returns an error if database operations fail.
pub fn store_symbols(
    conn: &Connection,
    file_path: &str,
    file_hash: &str,
    symbols: &[ExtractedSymbol],
) -> Result<()> {
    // Delete old symbols for this file (cascade deletes structural_edges)
    conn.execute(
        "DELETE FROM structural_edges WHERE source_symbol_id IN (SELECT id FROM symbols WHERE file_path = ?1)",
        [file_path],
    )
    .map_err(|e| StorageError::Database(format!("failed to delete old structural edges: {e}")))?;

    conn.execute("DELETE FROM symbols WHERE file_path = ?1", [file_path])
        .map_err(|e| StorageError::Database(format!("failed to delete old symbols: {e}")))?;

    #[allow(clippy::cast_possible_wrap)]
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for symbol in symbols {
        #[allow(clippy::cast_possible_wrap)]
        let start = symbol.start_line as i32;
        #[allow(clippy::cast_possible_wrap)]
        let end = symbol.end_line as i32;

        conn.execute(
            "INSERT INTO symbols (file_path, symbol_name, symbol_kind, language, start_line, end_line, scope, signature, file_hash, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                file_path,
                symbol.name,
                symbol.kind.as_str(),
                symbol.language,
                start,
                end,
                symbol.scope,
                symbol.signature,
                file_hash,
                now,
            ],
        )
        .map_err(|e| StorageError::Database(format!("failed to insert symbol: {e}")))?;
    }

    Ok(())
}

/// Store structural edges for a source symbol.
///
/// `edges` is a slice of `(target_name, target_file_path, edge_kind)` tuples.
///
/// # Errors
///
/// Returns an error if database operations fail.
pub fn store_structural_edges(
    conn: &Connection,
    source_id: i64,
    edges: &[(String, Option<String>, String)],
) -> Result<()> {
    #[allow(clippy::cast_possible_wrap)]
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for (target_name, target_file, edge_kind) in edges {
        conn.execute(
            "INSERT INTO structural_edges (source_symbol_id, target_symbol_name, target_file_path, edge_kind, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![source_id, target_name, target_file, edge_kind, now],
        )
        .map_err(|e| StorageError::Database(format!("failed to insert structural edge: {e}")))?;
    }

    Ok(())
}

/// Query symbols by name (exact match).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn query_symbols_by_name(conn: &Connection, name: &str) -> Result<Vec<SymbolRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, file_path, symbol_name, symbol_kind, language, start_line, end_line, scope, signature, file_hash, indexed_at
             FROM symbols WHERE symbol_name = ?1",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let rows = stmt
        .query_map([name], map_symbol_row)
        .map_err(|e| StorageError::Database(format!("failed to query symbols: {e}")))?;

    collect_symbol_rows(rows)
}

/// Query symbols by file path.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn query_symbols_by_file(conn: &Connection, file_path: &str) -> Result<Vec<SymbolRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, file_path, symbol_name, symbol_kind, language, start_line, end_line, scope, signature, file_hash, indexed_at
             FROM symbols WHERE file_path = ?1",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let rows = stmt
        .query_map([file_path], map_symbol_row)
        .map_err(|e| StorageError::Database(format!("failed to query symbols: {e}")))?;

    collect_symbol_rows(rows)
}

/// Find symbols that call the given target symbol name.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn query_callers(conn: &Connection, symbol_name: &str) -> Result<Vec<SymbolRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.file_path, s.symbol_name, s.symbol_kind, s.language, s.start_line, s.end_line, s.scope, s.signature, s.file_hash, s.indexed_at
             FROM symbols s
             INNER JOIN structural_edges e ON e.source_symbol_id = s.id
             WHERE e.target_symbol_name = ?1 AND e.edge_kind = 'calls'",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let rows = stmt
        .query_map([symbol_name], map_symbol_row)
        .map_err(|e| StorageError::Database(format!("failed to query callers: {e}")))?;

    collect_symbol_rows(rows)
}

/// Find symbols called by the given source symbol name.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn query_callees(conn: &Connection, symbol_name: &str) -> Result<Vec<SymbolRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT s2.id, s2.file_path, s2.symbol_name, s2.symbol_kind, s2.language, s2.start_line, s2.end_line, s2.scope, s2.signature, s2.file_hash, s2.indexed_at
             FROM symbols s1
             INNER JOIN structural_edges e ON e.source_symbol_id = s1.id
             INNER JOIN symbols s2 ON s2.symbol_name = e.target_symbol_name
             WHERE s1.symbol_name = ?1 AND e.edge_kind = 'calls'",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let rows = stmt
        .query_map([symbol_name], map_symbol_row)
        .map_err(|e| StorageError::Database(format!("failed to query callees: {e}")))?;

    collect_symbol_rows(rows)
}

/// Find symbols that import the given symbol.
///
/// Queries structural_edges where target_symbol_name matches and edge_kind = 'imports'.
/// Returns the source symbols (the ones doing the importing).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn query_importers(conn: &Connection, symbol_name: &str) -> Result<Vec<SymbolRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.file_path, s.symbol_name, s.symbol_kind, s.language, s.start_line, s.end_line, s.scope, s.signature, s.file_hash, s.indexed_at
             FROM symbols s
             INNER JOIN structural_edges e ON e.source_symbol_id = s.id
             WHERE e.target_symbol_name = ?1 AND e.edge_kind = 'imports'",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let rows = stmt
        .query_map([symbol_name], map_symbol_row)
        .map_err(|e| StorageError::Database(format!("failed to query importers: {e}")))?;

    collect_symbol_rows(rows)
}

/// Find symbols that are inherited by the given symbol.
///
/// Queries structural_edges where target_symbol_name matches and edge_kind = 'inherits'.
/// Returns the source symbols (the ones doing the inheriting).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn query_inheritors(conn: &Connection, symbol_name: &str) -> Result<Vec<SymbolRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.file_path, s.symbol_name, s.symbol_kind, s.language, s.start_line, s.end_line, s.scope, s.signature, s.file_hash, s.indexed_at
             FROM symbols s
             INNER JOIN structural_edges e ON e.source_symbol_id = s.id
             WHERE e.target_symbol_name = ?1 AND e.edge_kind = 'inherits'",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare query: {e}")))?;

    let rows = stmt
        .query_map([symbol_name], map_symbol_row)
        .map_err(|e| StorageError::Database(format!("failed to query inheritors: {e}")))?;

    collect_symbol_rows(rows)
}

/// Count total symbols in the database.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn count_symbols(conn: &Connection) -> Result<i64> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .map_err(|e| StorageError::Database(format!("failed to count symbols: {e}")))?;
    Ok(count)
}

/// Check if symbols exist for a file with the given hash (for incremental re-index).
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn has_symbols_for_hash(conn: &Connection, file_path: &str, file_hash: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE file_path = ?1 AND file_hash = ?2",
            rusqlite::params![file_path, file_hash],
            |row| row.get(0),
        )
        .map_err(|e| StorageError::Database(format!("failed to check symbols: {e}")))?;
    Ok(count > 0)
}

/// Map a database row to a `SymbolRecord`.
fn map_symbol_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolRecord> {
    Ok(SymbolRecord {
        id: row.get(0)?,
        file_path: row.get(1)?,
        symbol_name: row.get(2)?,
        symbol_kind: SymbolKind::parse(&row.get::<_, String>(3)?).unwrap_or(SymbolKind::Function),
        language: row.get(4)?,
        start_line: row.get(5)?,
        end_line: row.get(6)?,
        scope: row.get(7)?,
        signature: row.get(8)?,
        file_hash: row.get(9)?,
        indexed_at: row.get(10)?,
    })
}

/// Collect rows into a Vec, mapping errors.
fn collect_symbol_rows(
    rows: rusqlite::MappedRows<
        '_,
        impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<SymbolRecord>,
    >,
) -> Result<Vec<SymbolRecord>> {
    let mut result = Vec::new();
    for row in rows {
        result.push(
            row.map_err(|e| StorageError::Database(format!("failed to read symbol row: {e}")))?,
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{migrate, Database};
    use crate::structural::extractor::SymbolKind;

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| migrate(conn)).unwrap();
        db
    }

    fn make_symbol(name: &str, kind: SymbolKind) -> ExtractedSymbol {
        ExtractedSymbol {
            name: name.to_string(),
            kind,
            start_line: 0,
            end_line: 5,
            scope: None,
            signature: None,
            language: "python".to_string(),
        }
    }

    #[test]
    fn test_store_and_query_symbols() {
        let db = setup_db();
        let symbols = vec![
            make_symbol("hello", SymbolKind::Function),
            make_symbol("Foo", SymbolKind::Class),
        ];

        db.with_conn(|conn| {
            store_symbols(conn, "/test/main.py", "hash1", &symbols)?;
            let results = query_symbols_by_name(conn, "hello")?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].symbol_name, "hello");
            assert_eq!(results[0].symbol_kind, SymbolKind::Function);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_store_replaces_old_symbols() {
        let db = setup_db();
        let symbols_v1 = vec![make_symbol("old_func", SymbolKind::Function)];
        let symbols_v2 = vec![make_symbol("new_func", SymbolKind::Function)];

        db.with_conn(|conn| {
            store_symbols(conn, "/test/main.py", "hash1", &symbols_v1)?;
            store_symbols(conn, "/test/main.py", "hash2", &symbols_v2)?;
            let results = query_symbols_by_file(conn, "/test/main.py")?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].symbol_name, "new_func");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_query_callers() {
        let db = setup_db();
        let caller = make_symbol("caller_fn", SymbolKind::Function);
        let callee = make_symbol("callee_fn", SymbolKind::Function);

        db.with_conn(|conn| {
            store_symbols(conn, "/test/a.py", "hash1", &[caller])?;
            store_symbols(conn, "/test/b.py", "hash2", &[callee])?;

            // Get the caller's ID
            let caller_id: i64 = conn
                .query_row(
                    "SELECT id FROM symbols WHERE symbol_name = 'caller_fn'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();

            // Add a calls edge
            store_structural_edges(
                conn,
                caller_id,
                &[(
                    "callee_fn".to_string(),
                    Some("/test/b.py".to_string()),
                    "calls".to_string(),
                )],
            )?;

            let callers = query_callers(conn, "callee_fn")?;
            assert_eq!(callers.len(), 1);
            assert_eq!(callers[0].symbol_name, "caller_fn");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_count_symbols() {
        let db = setup_db();
        let symbols = vec![
            make_symbol("a", SymbolKind::Function),
            make_symbol("b", SymbolKind::Class),
        ];

        db.with_conn(|conn| {
            store_symbols(conn, "/test/main.py", "hash1", &symbols)?;
            let count = count_symbols(conn)?;
            assert_eq!(count, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_has_symbols_for_hash() {
        let db = setup_db();
        let symbols = vec![make_symbol("a", SymbolKind::Function)];

        db.with_conn(|conn| {
            store_symbols(conn, "/test/main.py", "hash1", &symbols)?;
            assert!(has_symbols_for_hash(conn, "/test/main.py", "hash1")?);
            assert!(!has_symbols_for_hash(conn, "/test/main.py", "hash2")?);
            assert!(!has_symbols_for_hash(conn, "/test/other.py", "hash1")?);
            Ok(())
        })
        .unwrap();
    }
}
