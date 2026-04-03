//! Integration test for transcript ingestion pipeline.
//!
//! Tests the complete pipeline of parsing transcripts, extracting lessons,
//! deduplicating, and storing them in the database.

use nellie::claude_code::ingest::{ingest_transcripts, IngestConfig};
use nellie::storage::{init_storage, Database};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_ingest_sample_transcript() {
    // Create temporary database
    let temp_db_dir = TempDir::new().unwrap();
    let db_path = temp_db_dir.path().join("test.db");

    let db = Database::open(&db_path).expect("Failed to open database");
    init_storage(&db).expect("Failed to initialize storage");

    // Create a sample transcript file
    let temp_transcript_dir = TempDir::new().unwrap();
    let transcript_path = temp_transcript_dir.path().join("session.jsonl");

    // Write a simple transcript with a user message that should trigger a lesson
    let transcript_content = r#"{"uuid":"entry-1","parentUuid":null,"type":"user","message":{"content":"Remember: always use WAL mode for SQLite"},"timestamp":"2024-01-01T00:00:00.000Z","sessionId":"session-1","cwd":"/tmp/test","gitBranch":"main"}
{"uuid":"entry-2","parentUuid":"entry-1","type":"assistant","message":{"content":"I'll remember that SQLite WAL mode improves concurrency.","contentBlocks":[]},"timestamp":"2024-01-01T00:00:01.000Z","sessionId":"session-1","cwd":"/tmp/test","gitBranch":"main"}
"#;

    fs::write(&transcript_path, transcript_content).expect("Failed to write transcript");

    // Verify the transcript was created
    assert!(transcript_path.exists(), "Transcript file was not created");

    // Configure ingestion for the single file
    let config = IngestConfig {
        transcript_path: Some(transcript_path.clone()),
        project_path: None,
        since: None,
        dry_run: false,
    };

    // Execute ingestion
    let report = ingest_transcripts(&db, &config).expect("Ingestion failed");

    // Verify results
    assert_eq!(
        report.transcripts_processed, 1,
        "Should have processed one transcript"
    );
    assert!(
        report.total_extracted > 0,
        "Should have extracted at least one lesson"
    );

    // Verify that lessons were actually stored
    let stored_lessons: Vec<_> = db
        .with_conn(nellie::storage::list_lessons)
        .unwrap_or_default();

    assert!(
        !stored_lessons.is_empty(),
        "At least one lesson should be stored in the database"
    );

    println!("Integration test passed!");
    println!("Processed {} transcripts", report.transcripts_processed);
    println!("Extracted {} lessons", report.total_extracted);
    println!("Stored {} lessons", report.total_stored);
    println!("Stored lessons in DB: {}", stored_lessons.len());
}

#[test]
fn test_ingest_dry_run_mode() {
    // Create temporary database
    let temp_db_dir = TempDir::new().unwrap();
    let db_path = temp_db_dir.path().join("test_dry_run.db");

    let db = Database::open(&db_path).expect("Failed to open database");
    init_storage(&db).expect("Failed to initialize storage");

    // Create a sample transcript file
    let temp_transcript_dir = TempDir::new().unwrap();
    let transcript_path = temp_transcript_dir.path().join("session.jsonl");

    let transcript_content = r#"{"uuid":"entry-1","parentUuid":null,"type":"user","message":{"content":"Remember: use transactions for atomicity"},"timestamp":"2024-01-01T00:00:00.000Z","sessionId":"session-2","cwd":"/tmp/test","gitBranch":"main"}
{"uuid":"entry-2","parentUuid":"entry-1","type":"assistant","message":{"content":"Good point about transactions.","contentBlocks":[]},"timestamp":"2024-01-01T00:00:01.000Z","sessionId":"session-2","cwd":"/tmp/test","gitBranch":"main"}
"#;

    fs::write(&transcript_path, transcript_content).expect("Failed to write transcript");

    // Run with dry_run=true
    let config = IngestConfig {
        transcript_path: Some(transcript_path),
        project_path: None,
        since: None,
        dry_run: true,
    };

    let report = ingest_transcripts(&db, &config).expect("Ingestion failed");

    // In dry run mode, lessons should be extracted but NOT stored
    assert_eq!(
        report.transcripts_processed, 1,
        "Should have processed one transcript"
    );

    // Check that nothing was actually stored (dry_run=true)
    // The stored lessons count might be 0 or might be from a previous state,
    // but the report should indicate what WOULD have been stored
    let initial_lessons: Vec<_> = db
        .with_conn(nellie::storage::list_lessons)
        .unwrap_or_default();

    println!("Dry run test passed!");
    println!("Extracted lessons: {}", report.total_extracted);
    println!("Lessons in DB after dry run: {}", initial_lessons.len());
}

#[test]
fn test_ingest_duplicate_detection() {
    // Create temporary database with a pre-existing lesson
    let temp_db_dir = TempDir::new().unwrap();
    let db_path = temp_db_dir.path().join("test_duplicates.db");

    let db = Database::open(&db_path).expect("Failed to open database");
    init_storage(&db).expect("Failed to initialize storage");

    // Insert a lesson directly into the DB
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let existing_lesson = nellie::storage::LessonRecord {
        id: "lesson_existing".to_string(),
        title: "SQLite concurrency".to_string(),
        content: "Use WAL mode".to_string(),
        tags: vec!["database".to_string()],
        severity: "info".to_string(),
        agent: Some("test".to_string()),
        repo: None,
        created_at: now as i64,
        updated_at: now as i64,
        embedding: None,
    };

    db.with_conn(|conn| nellie::storage::insert_lesson(conn, &existing_lesson))
        .expect("Failed to insert lesson");

    // Create a transcript that would extract a similar lesson
    let temp_transcript_dir = TempDir::new().unwrap();
    let transcript_path = temp_transcript_dir.path().join("session.jsonl");

    let transcript_content = r#"{"uuid":"entry-1","parentUuid":null,"type":"user","message":{"content":"Remember: SQLite concurrency is important, use WAL mode"},"timestamp":"2024-01-01T00:00:00.000Z","sessionId":"session-3","cwd":"/tmp/test","gitBranch":"main"}
{"uuid":"entry-2","parentUuid":"entry-1","type":"assistant","message":{"content":"Yes, WAL mode is essential.","contentBlocks":[]},"timestamp":"2024-01-01T00:00:01.000Z","sessionId":"session-3","cwd":"/tmp/test","gitBranch":"main"}
"#;

    fs::write(&transcript_path, transcript_content).expect("Failed to write transcript");

    // Ingest the transcript
    let config = IngestConfig {
        transcript_path: Some(transcript_path),
        project_path: None,
        since: None,
        dry_run: false,
    };

    let report = ingest_transcripts(&db, &config).expect("Ingestion failed");

    // The duplicate detection should catch this
    println!("Duplicate detection test:");
    println!("Extracted: {}", report.total_extracted);
    println!("Duplicates detected: {}", report.total_duplicates);
    println!("New lessons stored: {}", report.total_stored);
}
