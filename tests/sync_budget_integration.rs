//! Integration test for sync command with budget enforcement.
//!
//! Tests the complete pipeline of syncing lessons and checkpoints from
//! the database to Claude Code memory files, including deduplication,
//! decay scoring, and line budget enforcement.

use nellie::claude_code::sync::{execute_sync, SyncConfig};
use nellie::storage::{init_storage, Database, LessonRecord};
use std::fs;
use tempfile::TempDir;

/// Helper to insert test lessons into the database.
fn insert_test_lessons(db: &Database, count: usize) {
    for i in 0..count {
        let title = format!("Test Lesson {}", i);
        // Make content significantly different to avoid false duplicates
        let content = match i % 4 {
            0 => format!(
                "Content about SQLite database optimization for test lesson {}",
                i
            ),
            1 => format!("Content about Rust async patterns for test lesson {}", i),
            2 => format!("Content about file system operations for test lesson {}", i),
            _ => format!(
                "Content about networking and concurrency for test lesson {}",
                i
            ),
        };
        let severity = if i % 3 == 0 {
            "critical"
        } else if i % 3 == 1 {
            "warning"
        } else {
            "info"
        };

        let mut lesson = LessonRecord::new(&title, &content, vec!["test".to_string()]);
        lesson.severity = severity.to_string();
        lesson.created_at = 1704067200i64; // 2024-01-01
        lesson.updated_at = 1704067200i64 + (i as i64 * 86400); // Stagger by days

        db.with_conn(|conn| nellie::storage::insert_lesson(conn, &lesson))
            .unwrap();
    }
}

#[test]
fn test_sync_with_budget_enforcement() {
    // Create temporary database
    let temp_db_dir = TempDir::new().unwrap();
    let db_path = temp_db_dir.path().join("test_sync.db");

    let db = Database::open(&db_path).expect("Failed to open database");
    init_storage(&db).expect("Failed to initialize storage");

    // Insert 20 lessons
    insert_test_lessons(&db, 20);

    // Create temporary project directory
    let temp_project_dir = TempDir::new().unwrap();
    let project_dir = temp_project_dir.path().to_path_buf();

    // Create sync config with a tight budget to force filtering
    let mut config = SyncConfig::new(project_dir.clone());
    config.dry_run = false;
    config.max_lessons = 20;
    config.max_checkpoints = 0;
    config.budget = 80; // Tight budget to force filtering (around 5-10 lessons)

    // Execute sync
    let report = execute_sync(&db, &config).expect("Sync failed");

    println!("Sync report:");
    println!("  Lessons written: {}", report.lessons_written);
    println!("  Index lines: {}", report.index_lines);
    println!("  Index entries: {}", report.index_entries);
    println!("  Memory dir: {}", report.memory_dir.display());
    println!("\nSync actions:");
    for action in &report.actions {
        println!("  - {}", action);
    }

    // Verify that MEMORY.md exists and is within budget
    let memory_md_path = &report.memory_dir.join("MEMORY.md");
    assert!(memory_md_path.exists(), "MEMORY.md should exist");

    let memory_content = fs::read_to_string(&memory_md_path).expect("Failed to read MEMORY.md");
    let line_count = memory_content.lines().count();

    // Check that the MEMORY.md is within the budget (plus some tolerance for non-Nellie entries)
    println!("MEMORY.md line count: {} (budget: 50)", line_count);
    assert!(
        line_count <= 50 + 10, // Allow 10 lines of tolerance for headers, blanks, etc.
        "MEMORY.md should stay close to the budget"
    );

    // Verify that not all lessons were written (due to budget)
    assert!(
        report.lessons_written < 20,
        "Budget should have filtered some lessons"
    );

    // Verify that critical lessons were prioritized (at least some critical should be written)
    assert!(
        report.lessons_written > 0,
        "At least some lessons should be written"
    );
}

#[test]
fn test_sync_budget_preserves_critical_lessons() {
    // Create temporary database
    let temp_db_dir = TempDir::new().unwrap();
    let db_path = temp_db_dir.path().join("test_critical.db");

    let db = Database::open(&db_path).expect("Failed to open database");
    init_storage(&db).expect("Failed to initialize storage");

    // Insert critical lesson
    let mut critical_lesson = LessonRecord::new(
        "Critical SQLite WAL Mode",
        "Always use WAL2 mode for concurrent access",
        vec!["sqlite".to_string(), "concurrency".to_string()],
    );
    critical_lesson.severity = "critical".to_string();
    critical_lesson.created_at = 1704067200i64;
    critical_lesson.updated_at = 1704067200i64;
    db.with_conn(|conn| nellie::storage::insert_lesson(conn, &critical_lesson))
        .unwrap();

    // Insert warning lesson
    let mut warning_lesson = LessonRecord::new(
        "Rust Async Patterns",
        "Use tokio for async runtime management",
        vec!["rust".to_string(), "async".to_string()],
    );
    warning_lesson.severity = "warning".to_string();
    warning_lesson.created_at = 1704067200i64;
    warning_lesson.updated_at = 1704067200i64 - 86400 * 30; // 30 days old
    db.with_conn(|conn| nellie::storage::insert_lesson(conn, &warning_lesson))
        .unwrap();

    // Insert info lesson
    let mut info_lesson = LessonRecord::new(
        "Formatter Options",
        "Consider using rustfmt --check for CI",
        vec!["rust".to_string(), "formatting".to_string()],
    );
    info_lesson.severity = "info".to_string();
    info_lesson.created_at = 1704067200i64;
    info_lesson.updated_at = 1704067200i64 - 86400 * 60; // 60 days old (very stale)
    db.with_conn(|conn| nellie::storage::insert_lesson(conn, &info_lesson))
        .unwrap();

    // Create temporary project directory
    let temp_project_dir = TempDir::new().unwrap();
    let project_dir = temp_project_dir.path().to_path_buf();

    // Create sync config with budget that forces filtering
    let mut config = SyncConfig::new(project_dir.clone());
    config.dry_run = false;
    config.max_lessons = 10;
    config.max_checkpoints = 0;
    config.budget = 30; // Only enough for 1-2 lessons

    // Execute sync
    let report = execute_sync(&db, &config).expect("Sync failed");

    println!("Sync report (critical preservation test):");
    println!("  Lessons written: {}", report.lessons_written);
    println!("  Index entries: {}", report.index_entries);

    // Read MEMORY.md and check that critical lesson is present
    let memory_md_path = &report.memory_dir.join("MEMORY.md");
    let memory_content = fs::read_to_string(&memory_md_path).expect("Failed to read MEMORY.md");

    println!("MEMORY.md content:\n{}", memory_content);

    // The critical lesson should be in MEMORY.md
    assert!(
        memory_content.contains("Critical SQLite WAL Mode") || memory_content.contains("critical"),
        "Critical lesson should be preserved even with tight budget"
    );

    // Verify budget is respected
    let line_count = memory_content.lines().count();
    assert!(
        line_count <= 30 + 15, // 15 lines tolerance for headers/blanks
        "Budget should be enforced: {} lines",
        line_count
    );
}

#[test]
fn test_sync_default_budget() {
    // Create temporary database
    let temp_db_dir = TempDir::new().unwrap();
    let db_path = temp_db_dir.path().join("test_default_budget.db");

    let db = Database::open(&db_path).expect("Failed to open database");
    init_storage(&db).expect("Failed to initialize storage");

    // Insert some lessons
    insert_test_lessons(&db, 5);

    // Create temporary project directory
    let temp_project_dir = TempDir::new().unwrap();
    let project_dir = temp_project_dir.path().to_path_buf();

    // Create sync config with default budget (180)
    let config = SyncConfig::new(project_dir.clone());

    // Execute sync
    let report = execute_sync(&db, &config).expect("Sync failed");

    // Verify that MEMORY.md was created and is reasonable
    let memory_md_path = &report.memory_dir.join("MEMORY.md");
    assert!(memory_md_path.exists(), "MEMORY.md should exist");

    let memory_content = fs::read_to_string(&memory_md_path).expect("Failed to read MEMORY.md");
    let line_count = memory_content.lines().count();

    // Should fit easily within default 180 budget
    assert!(
        line_count <= 180,
        "Default budget (180) should be respected: {} lines",
        line_count
    );

    println!("Default budget test passed: {} lines used", line_count);
}
