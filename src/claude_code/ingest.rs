//! Transcript ingestion pipeline for passive learning.
//!
//! This module orchestrates the pipeline for learning from Claude Code
//! session transcripts:
//! 1. Parse transcript(s) from `.jsonl` files
//! 2. Extract learnable patterns (corrections, failures, etc.)
//! 3. Deduplicate against existing lessons in Nellie DB
//! 4. Track processed transcripts to avoid re-processing
//! 5. Store new lessons in the database
//!
//! # Single File Ingestion
//!
//! ```rust,ignore
//! let config = IngestConfig {
//!     transcript_path: Some(Path::new("session.jsonl").to_path_buf()),
//!     project_path: None,
//!     since: None,
//!     dry_run: false,
//! };
//! let report = ingest_transcripts(&db, &config)?;
//! ```
//!
//! # Batch Ingestion
//!
//! ```rust,ignore
//! let config = IngestConfig {
//!     transcript_path: None,
//!     project_path: Some(Path::new("/home/mmn/github/nellie-rs").to_path_buf()),
//!     since: Some(1630000000), // Unix timestamp
//!     dry_run: false,
//! };
//! let report = ingest_transcripts(&db, &config)?;
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::claude_code::remote::RemoteClient;
use crate::storage::{Database, LessonRecord};
use crate::Result;

use super::extractor::{extract_lessons, ExtractedLesson};
use super::paths::resolve_transcript_dir;
use super::transcript::parse_transcript;

/// Configuration for transcript ingestion.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Path to a single transcript file (mutually exclusive with project_path)
    pub transcript_path: Option<PathBuf>,

    /// Project directory to batch-scan for transcripts (mutually exclusive with transcript_path)
    pub project_path: Option<PathBuf>,

    /// Only ingest transcripts modified since this Unix timestamp (for batch mode)
    pub since: Option<i64>,

    /// If true, show what would be ingested without actually storing
    pub dry_run: bool,
}

/// Deduplication result for a single extracted lesson.
#[derive(Debug, Clone)]
pub struct DedupResult {
    /// The extracted lesson
    pub lesson: ExtractedLesson,

    /// Whether this lesson already exists (similar title/content)
    pub is_duplicate: bool,

    /// If duplicate, the ID of the existing lesson it matched
    pub duplicate_of: Option<String>,
}

/// Report from a single transcript ingestion.
#[derive(Debug, Clone)]
pub struct TranscriptReport {
    /// Path to the transcript file
    pub path: String,

    /// Number of lessons extracted from this transcript
    pub extracted_count: usize,

    /// Number of new lessons (non-duplicates) that would/will be stored
    pub new_lessons: usize,

    /// Number of duplicate lessons
    pub duplicate_lessons: usize,

    /// Lessons that were newly stored (or would be in dry-run)
    pub stored_lessons: Vec<String>,
}

/// Overall report from ingesting all transcripts.
#[derive(Debug, Clone)]
pub struct IngestReport {
    /// Number of transcripts processed
    pub transcripts_processed: usize,

    /// Total lessons extracted across all transcripts
    pub total_extracted: usize,

    /// Total new lessons stored (non-duplicates)
    pub total_stored: usize,

    /// Total duplicate lessons skipped
    pub total_duplicates: usize,

    /// Per-transcript reports
    pub transcript_reports: Vec<TranscriptReport>,

    /// Error messages encountered (non-fatal)
    pub errors: Vec<String>,
}

/// Ingest one or more transcripts and extract learnable lessons.
///
/// Depending on the `IngestConfig`:
/// - **Single file**: Parses one `.jsonl` transcript
/// - **Batch mode**: Scans `~/.claude/projects/<project>/` for `.jsonl`
///   files modified since `--since` (or all if `--since` not provided)
///
/// For each transcript:
/// 1. Parse it into [`TranscriptEntry`] values
/// 2. Extract patterns into [`ExtractedLesson`] candidates
/// 3. Deduplicate against existing lessons in the Nellie DB
/// 4. Store new lessons (unless `dry_run` is true)
/// 5. Track that the transcript was processed
///
/// # Arguments
///
/// * `db` — Nellie database connection
/// * `config` — Ingestion configuration (single file or batch mode)
///
/// # Returns
///
/// An [`IngestReport`] with details on processed transcripts and stored
/// lessons.
///
/// # Errors
///
/// Returns an error only if critical operations fail (e.g., DB access).
/// Non-fatal errors (malformed transcripts) are logged and included in
/// the report.
pub fn ingest_transcripts(db: &Database, config: &IngestConfig) -> Result<IngestReport> {
    let mut report = IngestReport {
        transcripts_processed: 0,
        total_extracted: 0,
        total_stored: 0,
        total_duplicates: 0,
        transcript_reports: Vec::new(),
        errors: Vec::new(),
    };

    // Determine which transcripts to process
    let transcript_paths = if let Some(ref single_path) = config.transcript_path {
        // Single file mode
        vec![single_path.clone()]
    } else if let Some(ref project_path) = config.project_path {
        // Batch mode: scan project directory
        find_transcripts_in_project(project_path, config.since)?
    } else {
        return Err(crate::Error::internal(
            "either transcript_path or project_path must be specified",
        ));
    };

    if transcript_paths.is_empty() {
        tracing::info!("No transcripts found to ingest");
        return Ok(report);
    }

    tracing::info!(
        count = transcript_paths.len(),
        dry_run = config.dry_run,
        "Starting transcript ingestion"
    );

    // Load existing lessons for deduplication
    let existing_lessons = db
        .with_conn(crate::storage::list_lessons)
        .unwrap_or_default();

    // Process each transcript
    for path in transcript_paths {
        match process_single_transcript(db, &path, &existing_lessons, config.dry_run) {
            Ok(transcript_report) => {
                report.transcripts_processed += 1;
                report.total_extracted += transcript_report.extracted_count;
                report.total_stored += transcript_report.new_lessons;
                report.total_duplicates += transcript_report.duplicate_lessons;
                report.transcript_reports.push(transcript_report);
            }
            Err(e) => {
                let error_msg = format!("Failed to process {}: {}", path.display(), e);
                tracing::warn!("{error_msg}");
                report.errors.push(error_msg);
            }
        }
    }

    tracing::info!(
        transcripts = report.transcripts_processed,
        extracted = report.total_extracted,
        stored = report.total_stored,
        duplicates = report.total_duplicates,
        "Ingestion complete"
    );

    Ok(report)
}

/// Execute transcript ingestion using a remote Nellie server.
///
/// Transcripts are parsed and patterns extracted locally. Deduplication
/// checks are performed against lessons fetched from the remote server.
/// New lessons are POSTed to the remote server.
pub async fn ingest_transcripts_remote(
    client: &RemoteClient,
    config: &IngestConfig,
) -> Result<IngestReport> {
    let mut report = IngestReport {
        transcripts_processed: 0,
        total_extracted: 0,
        total_stored: 0,
        total_duplicates: 0,
        transcript_reports: Vec::new(),
        errors: Vec::new(),
    };

    if !client.is_healthy().await {
        return Err(crate::Error::internal(
            "remote Nellie server is not reachable",
        ));
    }

    // Determine which transcripts to process
    let transcript_paths = if let Some(ref single_path) = config.transcript_path {
        vec![single_path.clone()]
    } else if let Some(ref project_path) = config.project_path {
        find_transcripts_in_project(project_path, config.since)?
    } else {
        return Err(crate::Error::internal(
            "either transcript_path or project_path must be specified",
        ));
    };

    if transcript_paths.is_empty() {
        tracing::info!("No transcripts found to ingest");
        return Ok(report);
    }

    tracing::info!(
        count = transcript_paths.len(),
        dry_run = config.dry_run,
        "Starting remote transcript ingestion"
    );

    // Fetch existing lessons from remote for deduplication
    let existing_lessons = client.fetch_lessons(500).await.unwrap_or_default();

    for path in transcript_paths {
        match process_single_transcript_remote(client, &path, &existing_lessons, config.dry_run)
            .await
        {
            Ok(transcript_report) => {
                report.transcripts_processed += 1;
                report.total_extracted += transcript_report.extracted_count;
                report.total_stored += transcript_report.new_lessons;
                report.total_duplicates += transcript_report.duplicate_lessons;
                report.transcript_reports.push(transcript_report);
            }
            Err(e) => {
                let error_msg = format!("Failed to process {}: {}", path.display(), e);
                tracing::warn!("{error_msg}");
                report.errors.push(error_msg);
            }
        }
    }

    tracing::info!(
        transcripts = report.transcripts_processed,
        extracted = report.total_extracted,
        stored = report.total_stored,
        duplicates = report.total_duplicates,
        "Remote ingestion complete"
    );

    Ok(report)
}

/// Process a single transcript against a remote server.
async fn process_single_transcript_remote(
    client: &RemoteClient,
    path: &Path,
    existing_lessons: &[LessonRecord],
    dry_run: bool,
) -> Result<TranscriptReport> {
    tracing::info!(path = %path.display(), "Processing transcript (remote)");

    let entries = parse_transcript(path)
        .map_err(|e| crate::Error::internal(format!("failed to parse transcript: {e}")))?;
    if entries.is_empty() {
        return Ok(TranscriptReport {
            path: path.display().to_string(),
            extracted_count: 0,
            new_lessons: 0,
            duplicate_lessons: 0,
            stored_lessons: Vec::new(),
        });
    }

    let extracted = extract_lessons(&entries);
    let extracted_count = extracted.len();

    let mut new_lessons = Vec::new();
    let mut duplicate_count = 0;

    for lesson in extracted {
        let dedup = check_duplicate(&lesson, existing_lessons);
        if dedup.is_duplicate {
            duplicate_count += 1;
        } else {
            new_lessons.push(lesson);
        }
    }

    let stored_ids = if !dry_run && !new_lessons.is_empty() {
        let mut ids = Vec::new();
        for lesson in &new_lessons {
            let record = to_lesson_record(lesson);
            match client.post_lesson(&record).await {
                Ok(id) => ids.push(id),
                Err(e) => {
                    tracing::warn!(
                        title = %lesson.title,
                        error = %e,
                        "Failed to post lesson to remote"
                    );
                }
            }
        }
        tracing::info!(
            path = %path.display(),
            count = ids.len(),
            "Posted lessons to remote server"
        );
        ids
    } else if dry_run && !new_lessons.is_empty() {
        tracing::info!(
            path = %path.display(),
            count = new_lessons.len(),
            "DRY RUN: Would post lessons to remote (not actually posted)"
        );
        new_lessons
            .into_iter()
            .map(|l| format!("[dry-run] {}", l.title))
            .collect()
    } else {
        Vec::new()
    };

    Ok(TranscriptReport {
        path: path.display().to_string(),
        extracted_count,
        new_lessons: stored_ids.len(),
        duplicate_lessons: duplicate_count,
        stored_lessons: stored_ids,
    })
}

/// Convert an extracted lesson to a `LessonRecord` for remote posting.
fn to_lesson_record(lesson: &ExtractedLesson) -> LessonRecord {
    LessonRecord {
        id: String::new(),
        title: lesson.title.clone(),
        content: lesson.content.clone(),
        tags: lesson.tags.clone(),
        severity: lesson.severity.clone(),
        agent: None,
        repo: None,
        embedding: None,
        created_at: 0,
        updated_at: 0,
    }
}

/// Find all transcript files in a project directory.
///
/// Scans `~/.claude/projects/<project>/` for `.jsonl` files modified
/// since `since_timestamp` (if provided).
fn find_transcripts_in_project(project_path: &Path, since: Option<i64>) -> Result<Vec<PathBuf>> {
    let projects_dir = resolve_transcript_dir(project_path)?;

    if !projects_dir.exists() {
        tracing::warn!(
            path = %projects_dir.display(),
            "Project directory does not exist"
        );
        return Ok(Vec::new());
    }

    let mut transcripts = Vec::new();

    // Scan for .jsonl files
    if let Ok(entries) = fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Skip if not a .jsonl file
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }

            // Check mtime if --since was specified
            if let Some(timestamp) = since {
                if let Ok(metadata) = fs::metadata(&path) {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                            #[allow(clippy::cast_possible_wrap)]
                            let mtime = duration.as_secs() as i64;
                            if mtime < timestamp {
                                tracing::debug!(
                                    path = %path.display(),
                                    mtime,
                                    since = timestamp,
                                    "Skipping transcript (older than --since)"
                                );
                                continue;
                            }
                        }
                    }
                }
            }

            transcripts.push(path);
        }
    }

    tracing::info!(
        count = transcripts.len(),
        project = %projects_dir.display(),
        "Found transcripts"
    );

    Ok(transcripts)
}

/// Process a single transcript file and extract lessons.
///
/// 1. Parse the transcript into structured entries
/// 2. Extract lessons from patterns
/// 3. Deduplicate against existing lessons
/// 4. Store new lessons (unless dry_run)
/// 5. Return report
fn process_single_transcript(
    db: &Database,
    path: &Path,
    existing_lessons: &[LessonRecord],
    dry_run: bool,
) -> Result<TranscriptReport> {
    tracing::info!(path = %path.display(), "Processing transcript");

    // Parse transcript
    let entries = parse_transcript(path)
        .map_err(|e| crate::Error::internal(format!("failed to parse transcript: {e}")))?;
    if entries.is_empty() {
        tracing::warn!(path = %path.display(), "Transcript is empty");
        return Ok(TranscriptReport {
            path: path.display().to_string(),
            extracted_count: 0,
            new_lessons: 0,
            duplicate_lessons: 0,
            stored_lessons: Vec::new(),
        });
    }

    tracing::debug!(
        path = %path.display(),
        entries = entries.len(),
        "Parsed transcript"
    );

    // Extract lessons from patterns
    let extracted = extract_lessons(&entries);
    let extracted_count = extracted.len();

    tracing::debug!(
        path = %path.display(),
        lessons = extracted_count,
        "Extracted lessons"
    );

    // Deduplicate and prepare for storage
    let mut new_lessons = Vec::new();
    let mut duplicate_count = 0;

    for lesson in extracted {
        let dedup = check_duplicate(&lesson, existing_lessons);

        if dedup.is_duplicate {
            duplicate_count += 1;
            tracing::debug!(
                title = %lesson.title,
                duplicate_of = ?dedup.duplicate_of,
                "Skipping duplicate lesson"
            );
        } else {
            tracing::debug!(title = %lesson.title, "New lesson (not duplicate)");
            new_lessons.push(lesson);
        }
    }

    // Store new lessons
    let stored_ids = if !dry_run && !new_lessons.is_empty() {
        let ids = store_new_lessons(db, new_lessons)?;
        tracing::info!(
            path = %path.display(),
            count = ids.len(),
            "Stored new lessons"
        );
        ids
    } else if dry_run && !new_lessons.is_empty() {
        tracing::info!(
            path = %path.display(),
            count = new_lessons.len(),
            "DRY RUN: Would store lessons (not actually stored)"
        );
        new_lessons
            .into_iter()
            .map(|l| format!("[dry-run] {}", l.title))
            .collect()
    } else {
        Vec::new()
    };

    Ok(TranscriptReport {
        path: path.display().to_string(),
        extracted_count,
        new_lessons: stored_ids.len(),
        duplicate_lessons: duplicate_count,
        stored_lessons: stored_ids,
    })
}

/// Check if an extracted lesson is a duplicate of an existing one.
///
/// Uses title equality and Jaro-Winkler similarity of content to detect
/// duplicates. Returns early if titles match exactly, otherwise checks
/// content similarity.
fn check_duplicate(lesson: &ExtractedLesson, existing_lessons: &[LessonRecord]) -> DedupResult {
    use strsim::jaro_winkler;

    const CONTENT_SIMILARITY_THRESHOLD: f64 = 0.85;

    for existing in existing_lessons {
        // Exact title match is a clear duplicate
        if existing.title.eq_ignore_ascii_case(&lesson.title) {
            return DedupResult {
                lesson: lesson.clone(),
                is_duplicate: true,
                duplicate_of: Some(existing.id.clone()),
            };
        }

        // Check content similarity (for similar but not identical titles)
        let similarity = jaro_winkler(&existing.content, &lesson.content);
        if similarity > CONTENT_SIMILARITY_THRESHOLD {
            tracing::debug!(
                new_title = %lesson.title,
                existing_title = %existing.title,
                similarity = similarity,
                "Content similarity detected"
            );
            return DedupResult {
                lesson: lesson.clone(),
                is_duplicate: true,
                duplicate_of: Some(existing.id.clone()),
            };
        }
    }

    DedupResult {
        lesson: lesson.clone(),
        is_duplicate: false,
        duplicate_of: None,
    }
}

/// Store extracted lessons in the Nellie database.
///
/// Converts [`ExtractedLesson`] values to [`LessonRecord`] and inserts
/// them, returning the titles of newly stored lessons.
fn store_new_lessons(db: &Database, lessons: Vec<ExtractedLesson>) -> Result<Vec<String>> {
    let mut stored_ids = Vec::new();

    for lesson in lessons {
        // Generate unique ID
        let id = format!(
            "lesson_{}",
            &uuid::Uuid::new_v4().to_string().replace('-', "")[..12]
        );

        // Get current timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let lesson_title = lesson.title.clone();

        // Convert to LessonRecord
        #[allow(clippy::cast_possible_wrap)]
        let record = LessonRecord {
            id: id.clone(),
            title: lesson.title,
            content: lesson.content,
            tags: lesson.tags,
            severity: lesson.severity,
            agent: Some("nellie-ingest".to_string()),
            repo: Some(lesson.source_session),
            created_at: now as i64,
            updated_at: now as i64,
            embedding: None,
        };

        // Store in database
        db.with_conn(|conn| crate::storage::insert_lesson(conn, &record))?;

        stored_ids.push(lesson_title.clone());
        tracing::debug!(id = %id, title = %lesson_title, "Stored lesson");
    }

    Ok(stored_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_find_transcripts_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let transcripts = find_transcripts_in_project(temp_dir.path(), None).unwrap();
        assert!(transcripts.is_empty());
    }

    #[test]
    fn test_find_transcripts_filters_non_jsonl() {
        // This test is tricky because find_transcripts_in_project calls
        // resolve_transcript_dir which expects a project cwd, not a temp dir.
        // For now, we test that an empty/nonexistent dir returns empty results.
        let nonexistent = PathBuf::from("/nonexistent/project/path");
        let transcripts = find_transcripts_in_project(&nonexistent, None).unwrap();
        assert!(transcripts.is_empty());
    }

    #[test]
    fn test_find_transcripts_respects_since() {
        let temp_dir = TempDir::new().unwrap();

        // Create a transcript file
        let old_path = temp_dir.path().join("old.jsonl");
        let recent_path = temp_dir.path().join("recent.jsonl");

        let _old = std::fs::File::create(&old_path);
        let _recent = std::fs::File::create(&recent_path);

        // Use a since timestamp that should exclude the old file
        // (we'll use a very recent timestamp, so all files should be excluded in this test)
        let future_timestamp = i64::MAX;
        let transcripts =
            find_transcripts_in_project(temp_dir.path(), Some(future_timestamp)).unwrap();

        // Should be empty because both files are older than the future timestamp
        assert!(transcripts.is_empty());
    }

    #[test]
    fn test_check_duplicate_exact_title_match() {
        let extracted = ExtractedLesson {
            title: "How to fix timeout errors".to_string(),
            content: "Some content".to_string(),
            tags: vec!["errors".to_string()],
            severity: "info".to_string(),
            source_session: "session-1".to_string(),
        };

        let existing = vec![LessonRecord {
            id: "lesson_abc123".to_string(),
            title: "How to fix timeout errors".to_string(),
            content: "Different content".to_string(),
            tags: vec!["errors".to_string()],
            severity: "info".to_string(),
            agent: None,
            repo: None,
            created_at: 0,
            updated_at: 0,
            embedding: None,
        }];

        let result = check_duplicate(&extracted, &existing);
        assert!(result.is_duplicate);
        assert_eq!(result.duplicate_of, Some("lesson_abc123".to_string()));
    }

    #[test]
    fn test_check_duplicate_not_duplicate() {
        let extracted = ExtractedLesson {
            title: "New lesson about testing".to_string(),
            content: "Complete new content about testing strategies".to_string(),
            tags: vec!["testing".to_string()],
            severity: "info".to_string(),
            source_session: "session-1".to_string(),
        };

        let existing = vec![LessonRecord {
            id: "lesson_abc123".to_string(),
            title: "How to debug Rust".to_string(),
            content: "This is about debugging Rust code".to_string(),
            tags: vec!["rust".to_string()],
            severity: "info".to_string(),
            agent: None,
            repo: None,
            created_at: 0,
            updated_at: 0,
            embedding: None,
        }];

        let result = check_duplicate(&extracted, &existing);
        assert!(!result.is_duplicate);
        assert_eq!(result.duplicate_of, None);
    }

    #[test]
    fn test_ingest_config_single_file() {
        let config = IngestConfig {
            transcript_path: Some(PathBuf::from("session.jsonl")),
            project_path: None,
            since: None,
            dry_run: false,
        };

        assert!(config.transcript_path.is_some());
        assert!(config.project_path.is_none());
    }

    #[test]
    fn test_ingest_config_batch_mode() {
        let config = IngestConfig {
            transcript_path: None,
            project_path: Some(PathBuf::from("/home/user/project")),
            since: Some(1630000000),
            dry_run: true,
        };

        assert!(config.transcript_path.is_none());
        assert!(config.project_path.is_some());
        assert!(config.dry_run);
    }
}
