//! Lessons storage operations.

use rusqlite::{params, Connection};

use super::models::LessonRecord;
use crate::error::StorageError;
use crate::Result;

/// Insert a new lesson.
///
/// # Errors
///
/// Returns an error if JSON serialization or database insertion fails.
pub fn insert_lesson(conn: &Connection, lesson: &LessonRecord) -> Result<()> {
    let tags_json = serde_json::to_string(&lesson.tags)
        .map_err(|e| StorageError::Database(format!("failed to serialize tags: {e}")))?;

    conn.execute(
        "INSERT INTO lessons (id, title, content, tags, severity, agent, repo, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            lesson.id,
            lesson.title,
            lesson.content,
            tags_json,
            lesson.severity,
            lesson.agent,
            lesson.repo,
            lesson.created_at,
            lesson.updated_at,
        ],
    )
    .map_err(|e| StorageError::Database(format!("failed to insert lesson: {e}")))?;

    tracing::trace!(id = %lesson.id, "Inserted lesson");
    Ok(())
}

/// Get a lesson by ID.
///
/// # Errors
///
/// Returns an error if the lesson is not found or database query fails.
pub fn get_lesson(conn: &Connection, id: &str) -> Result<LessonRecord> {
    conn.query_row(
        "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
         FROM lessons WHERE id = ?",
        [id],
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
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound {
            entity: "lesson",
            id: id.to_string(),
        }
        .into(),
        e => StorageError::Database(format!("failed to get lesson: {e}")).into(),
    })
}

/// Update an existing lesson.
///
/// # Errors
///
/// Returns an error if the lesson is not found, JSON serialization fails, or database update fails.
pub fn update_lesson(conn: &Connection, lesson: &LessonRecord) -> Result<()> {
    let tags_json = serde_json::to_string(&lesson.tags)
        .map_err(|e| StorageError::Database(format!("failed to serialize tags: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let now_i64 = i64::try_from(now).unwrap_or_default();

    let rows = conn
        .execute(
            "UPDATE lessons SET title = ?, content = ?, tags = ?, severity = ?, updated_at = ?
             WHERE id = ?",
            params![
                lesson.title,
                lesson.content,
                tags_json,
                lesson.severity,
                now_i64,
                lesson.id
            ],
        )
        .map_err(|e| StorageError::Database(format!("failed to update lesson: {e}")))?;

    if rows == 0 {
        return Err(StorageError::NotFound {
            entity: "lesson",
            id: lesson.id.clone(),
        }
        .into());
    }

    Ok(())
}

/// Delete a lesson by ID.
///
/// # Errors
///
/// Returns an error if the lesson is not found or database deletion fails.
pub fn delete_lesson(conn: &Connection, id: &str) -> Result<()> {
    let rows = conn
        .execute("DELETE FROM lessons WHERE id = ?", [id])
        .map_err(|e| StorageError::Database(format!("failed to delete lesson: {e}")))?;

    if rows == 0 {
        return Err(StorageError::NotFound {
            entity: "lesson",
            id: id.to_string(),
        }
        .into());
    }

    Ok(())
}

/// List all lessons.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn list_lessons(conn: &Connection) -> Result<Vec<LessonRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
             FROM lessons ORDER BY created_at DESC",
        )
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

/// List lessons by severity.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn list_lessons_by_severity(conn: &Connection, severity: &str) -> Result<Vec<LessonRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
             FROM lessons WHERE severity = ? ORDER BY created_at DESC",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let lessons = stmt
        .query_map([severity], |row| {
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

/// List lessons by agent.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn list_lessons_by_agent(conn: &Connection, agent: &str) -> Result<Vec<LessonRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, content, tags, severity, agent, repo, created_at, updated_at
             FROM lessons WHERE agent = ? ORDER BY created_at DESC",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let lessons = stmt
        .query_map([agent], |row| {
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

/// Count total lessons.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn count_lessons(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM lessons", [], |row| row.get(0))
        .map_err(|e| StorageError::Database(e.to_string()).into())
}

/// List distinct repository names with lesson records.
///
/// Returns the set of unique `repo` values from the lessons table.
/// This is used for periodic sync to discover all known projects.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn list_distinct_repos(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT repo FROM lessons WHERE repo IS NOT NULL ORDER BY repo")
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let repos = stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for repo in repos {
        result.push(repo.map_err(|e| StorageError::Database(e.to_string()))?);
    }
    Ok(result)
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
    fn test_insert_and_get() {
        let db = setup_db();

        db.with_conn(|conn| {
            let lesson = LessonRecord::new(
                "Test Lesson",
                "This is a test lesson content",
                vec!["rust".to_string(), "testing".to_string()],
            )
            .with_severity("warning")
            .with_agent("test-agent");

            insert_lesson(conn, &lesson)?;

            let retrieved = get_lesson(conn, &lesson.id)?;
            assert_eq!(retrieved.title, "Test Lesson");
            assert_eq!(retrieved.tags, vec!["rust", "testing"]);
            assert_eq!(retrieved.severity, "warning");
            assert_eq!(retrieved.agent, Some("test-agent".to_string()));

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_update() {
        let db = setup_db();

        db.with_conn(|conn| {
            let mut lesson = LessonRecord::new("Original", "Content", vec![]);
            insert_lesson(conn, &lesson)?;

            lesson.title = "Updated".to_string();
            update_lesson(conn, &lesson)?;

            let retrieved = get_lesson(conn, &lesson.id)?;
            assert_eq!(retrieved.title, "Updated");

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_delete() {
        let db = setup_db();

        db.with_conn(|conn| {
            let lesson = LessonRecord::new("To Delete", "Content", vec![]);
            insert_lesson(conn, &lesson)?;

            delete_lesson(conn, &lesson.id)?;

            let result = get_lesson(conn, &lesson.id);
            assert!(result.is_err());

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_list_by_severity() {
        let db = setup_db();

        db.with_conn(|conn| {
            insert_lesson(
                conn,
                &LessonRecord::new("L1", "C1", vec![]).with_severity("critical"),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L2", "C2", vec![]).with_severity("warning"),
            )?;
            insert_lesson(
                conn,
                &LessonRecord::new("L3", "C3", vec![]).with_severity("critical"),
            )?;

            let critical = list_lessons_by_severity(conn, "critical")?;
            assert_eq!(critical.len(), 2);

            let warning = list_lessons_by_severity(conn, "warning")?;
            assert_eq!(warning.len(), 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_count() {
        let db = setup_db();

        db.with_conn(|conn| {
            assert_eq!(count_lessons(conn)?, 0);

            insert_lesson(conn, &LessonRecord::new("L1", "C1", vec![]))?;
            insert_lesson(conn, &LessonRecord::new("L2", "C2", vec![]))?;

            assert_eq!(count_lessons(conn)?, 2);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_list_distinct_repos() {
        let db = setup_db();

        db.with_conn(|conn| {
            // Empty DB
            let repos = list_distinct_repos(conn)?;
            assert_eq!(repos.len(), 0);

            // Insert lessons with different repos
            let mut l1 = LessonRecord::new("L1", "C1", vec![]);
            l1.repo = Some("repo-a".to_string());
            insert_lesson(conn, &l1)?;

            let mut l2 = LessonRecord::new("L2", "C2", vec![]);
            l2.repo = Some("repo-b".to_string());
            insert_lesson(conn, &l2)?;

            let mut l3 = LessonRecord::new("L3", "C3", vec![]);
            l3.repo = Some("repo-a".to_string());
            insert_lesson(conn, &l3)?;

            // Should have 2 distinct repos
            let repos = list_distinct_repos(conn)?;
            assert_eq!(repos.len(), 2);
            assert!(repos.contains(&"repo-a".to_string()));
            assert!(repos.contains(&"repo-b".to_string()));

            Ok(())
        })
        .unwrap();
    }
}
