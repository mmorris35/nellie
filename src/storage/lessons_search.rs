//! Lesson semantic search.

use rusqlite::Connection;

use super::models::{LessonRecord, SearchResult};
use crate::error::StorageError;
use crate::Result;

const LESSON_VEC_TABLE: &str = "lesson_embeddings";

/// Initialize lesson vector table.
///
/// # Errors
///
/// Returns an error if the table cannot be created.
pub fn init_lesson_vectors(conn: &Connection) -> Result<()> {
    let sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {LESSON_VEC_TABLE} USING vec0(
            id TEXT PRIMARY KEY,
            embedding FLOAT[384]
        )"
    );

    conn.execute(&sql, [])
        .map_err(|e| StorageError::Vector(format!("failed to create lesson vec table: {e}")))?;

    Ok(())
}

/// Store lesson embedding.
///
/// # Errors
///
/// Returns an error if the embedding cannot be stored.
pub fn store_lesson_embedding(conn: &Connection, lesson_id: &str, embedding: &[f32]) -> Result<()> {
    // Delete old embedding if exists
    conn.execute(
        &format!("DELETE FROM {LESSON_VEC_TABLE} WHERE id = ?"),
        [lesson_id],
    )
    .ok();

    // Insert new embedding
    let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    conn.execute(
        &format!("INSERT INTO {LESSON_VEC_TABLE} (id, embedding) VALUES (?, ?)"),
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

    let sql = format!(
        "SELECT id, distance FROM {LESSON_VEC_TABLE} WHERE embedding MATCH ? ORDER BY distance LIMIT ?"
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

/// Search lessons by text match (FTS or LIKE).
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
}
