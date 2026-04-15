//! Full sync command for writing Nellie knowledge to Claude Code memory files.
//!
//! The sync command orchestrates the entire flow of reading from Nellie's
//! database and writing to Claude Code's native file-based memory system.
//!
//! # Workflow
//!
//! 1. Resolve the target memory directory from `--project` (or CWD)
//! 2. Load existing MEMORY.md (or create a new one)
//! 3. Query lessons from Nellie DB (by severity: critical > warning > info)
//! 4. Query latest checkpoints per agent
//! 5. Write memory files and update the index
//! 6. Clean up stale Nellie-managed entries (lessons/checkpoints no longer in DB)
//! 7. Enforce the 200-line budget on MEMORY.md
//! 8. Report a summary of what was done
//! 9. (Optional) Sync rules to `~/.claude/rules/` when `--rules` is set
//!
//! # Rules Sync
//!
//! When `sync_rules` is enabled, the sync also generates glob-conditioned
//! rule files in `~/.claude/rules/` from critical and warning severity
//! lessons that have tags. Info-severity lessons are excluded from rules
//! generation (too noisy). Lessons without tags are also skipped since
//! there are no meaningful globs to condition on.
//!
//! Stale rule files (from lessons that no longer exist or no longer
//! qualify) are automatically cleaned up.
//!
//! # Dry-Run Mode
//!
//! When `--dry-run` is set, the command prints what it would do without
//! writing any files. This is useful for previewing the sync before
//! committing changes.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::claude_code::dedup::{
    find_duplicates, merge_duplicates, score_memory_relevance, select_memories_within_budget,
    ScoredMemory,
};
use crate::claude_code::mappers::{
    checkpoint_to_index_entry, checkpoint_to_memory, lesson_to_index_entry, lesson_to_memory,
};
use crate::claude_code::memory_index::{MemoryEntry, MemoryIndex, MAX_MEMORY_LINES};
use crate::claude_code::memory_writer::{delete_memory_file, write_memory_file, MemoryFile};
use crate::claude_code::paths::{resolve_project_memory_dir, resolve_rules_dir};
use crate::claude_code::remote::RemoteClient;
use crate::claude_code::rules::{clean_stale_rules, write_rule_file, TagGlobMapper};
use crate::storage::{
    get_latest_checkpoint, list_distinct_agents, list_lessons_by_severity, CheckpointRecord,
    Database, LessonRecord,
};

/// Default maximum number of lessons to sync.
pub const DEFAULT_MAX_LESSONS: usize = 50;

/// Default maximum number of checkpoints to sync (per agent).
pub const DEFAULT_MAX_CHECKPOINTS: usize = 3;

/// Configuration for a sync operation.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Project directory (used to resolve memory path).
    pub project_dir: PathBuf,

    /// If true, print what would be done without writing files.
    pub dry_run: bool,

    /// Maximum number of lessons to sync.
    pub max_lessons: usize,

    /// Maximum number of checkpoints to sync (per agent).
    pub max_checkpoints: usize,

    /// Maximum line budget for MEMORY.md index (default: 180).
    ///
    /// Claude Code truncates MEMORY.md at 200 lines, so the default
    /// reserves 20 lines for non-Nellie entries. After deduplication
    /// and scoring, only memories fitting within this budget are written.
    pub budget: usize,

    /// If true, also sync rules to `~/.claude/rules/`.
    ///
    /// Only critical and warning severity lessons with tags are
    /// converted to glob-conditioned rule files.
    pub sync_rules: bool,

    /// Override for the rules directory path.
    ///
    /// When `None`, defaults to `~/.claude/rules/` via
    /// [`resolve_rules_dir`]. Useful for testing with a temp
    /// directory.
    pub rules_dir_override: Option<PathBuf>,
}

impl SyncConfig {
    /// Creates a new `SyncConfig` with default limits.
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            dry_run: false,
            max_lessons: DEFAULT_MAX_LESSONS,
            max_checkpoints: DEFAULT_MAX_CHECKPOINTS,
            budget: 180,
            sync_rules: false,
            rules_dir_override: None,
        }
    }
}

/// Report of what the sync command did (or would do in dry-run mode).
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    /// Number of lesson memory files written.
    pub lessons_written: usize,

    /// Number of checkpoint memory files written.
    pub checkpoints_written: usize,

    /// Number of stale files cleaned up.
    pub stale_removed: usize,

    /// Number of index entries after sync.
    pub index_entries: usize,

    /// Total line count of MEMORY.md after sync.
    pub index_lines: usize,

    /// The resolved memory directory path.
    pub memory_dir: PathBuf,

    /// Number of rule files written (only when `--rules` is set).
    pub rules_written: usize,

    /// Number of stale rule files removed (only when `--rules` is set).
    pub rules_removed: usize,

    /// The resolved rules directory path (empty if rules sync disabled).
    pub rules_dir: PathBuf,

    /// Details about each action taken (useful for dry-run output).
    pub actions: Vec<String>,
}

impl std::fmt::Display for SyncReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Sync complete:")?;
        writeln!(f, "  Memory dir:          {}", self.memory_dir.display())?;
        writeln!(f, "  Lessons written:     {}", self.lessons_written)?;
        writeln!(f, "  Checkpoints written: {}", self.checkpoints_written)?;
        writeln!(f, "  Stale entries removed: {}", self.stale_removed)?;
        writeln!(f, "  Index entries:       {}", self.index_entries)?;
        writeln!(f, "  Index lines:         {}", self.index_lines)?;
        if self.rules_written > 0 || self.rules_removed > 0 {
            writeln!(f, "  Rules dir:           {}", self.rules_dir.display())?;
            writeln!(f, "  Rules written:       {}", self.rules_written)?;
            writeln!(f, "  Rules removed:       {}", self.rules_removed)?;
        }
        Ok(())
    }
}

/// Executes the full sync operation.
///
/// Queries lessons and checkpoints from the Nellie database, writes
/// them as Claude Code memory files, updates MEMORY.md, cleans up
/// stale entries, and enforces the 200-line budget.
///
/// # Arguments
///
/// * `db` - The Nellie database to read from.
/// * `config` - Sync configuration (project dir, limits, dry-run).
///
/// # Errors
///
/// Returns an error if database queries, path resolution, or file
/// I/O operations fail.
/// Execute sync using a local database.
pub fn execute_sync(db: &Database, config: &SyncConfig) -> crate::Result<SyncReport> {
    let lessons = query_lessons_by_priority(db, config.max_lessons)?;
    let checkpoints = query_latest_checkpoints(db, config.max_checkpoints)?;
    execute_sync_from_data(lessons, checkpoints, config)
}

/// Execute sync using a remote Nellie server.
///
/// Fetches lessons and checkpoints over HTTP, then runs the same
/// pipeline as the local sync — writing memory files and rules to
/// the local Claude Code directories.
pub async fn execute_sync_remote(
    client: &RemoteClient,
    config: &SyncConfig,
) -> crate::Result<SyncReport> {
    if !client.is_healthy().await {
        return Err(crate::Error::internal(
            "remote Nellie server is not reachable",
        ));
    }
    let lessons = client.fetch_lessons(config.max_lessons).await?;
    let checkpoints = client
        .fetch_latest_checkpoints(config.max_checkpoints)
        .await?;
    execute_sync_from_data(lessons, checkpoints, config)
}

/// Core sync pipeline shared by local and remote modes.
#[allow(clippy::needless_pass_by_value)]
fn execute_sync_from_data(
    lessons: Vec<LessonRecord>,
    checkpoints: Vec<CheckpointRecord>,
    config: &SyncConfig,
) -> crate::Result<SyncReport> {
    let mut report = SyncReport::default();

    // 1. Resolve target memory directory
    let memory_dir = resolve_project_memory_dir(&config.project_dir)?;
    report.memory_dir.clone_from(&memory_dir);

    let memory_md_path = memory_dir.join("MEMORY.md");

    // 2. Load existing MEMORY.md (or create new)
    let mut index = MemoryIndex::load(&memory_md_path)?;

    // 3. Report data source
    report.actions.push(format!(
        "Queried {} lessons (max: {})",
        lessons.len(),
        config.max_lessons
    ));

    report.actions.push(format!(
        "Queried {} checkpoints (max per agent: {})",
        checkpoints.len(),
        config.max_checkpoints
    ));

    // Track which filenames we write so we can identify stale entries
    let mut active_filenames: HashSet<String> = HashSet::new();

    // 5. Convert lessons to memory files and deduplicate
    let lesson_memory_files: Vec<_> = lessons.iter().map(lesson_to_memory).collect();
    let duplicate_groups = find_duplicates(&lesson_memory_files);

    report.actions.push(format!(
        "Deduplication: {} groups from {} lesson files",
        duplicate_groups.len(),
        lesson_memory_files.len()
    ));

    // 5a. Merge duplicates
    let mut merged_lessons: Vec<(String, String, String, MemoryFile)> = Vec::new();

    for group in duplicate_groups {
        if group.is_empty() {
            continue;
        }

        let merged = merge_duplicates(&group);

        // For tracking purposes, we use the merged file's title to look up
        // the original lesson's metadata. Find the best original lesson.
        let best_lesson = lessons
            .iter()
            .find(|l| l.title == merged.name)
            .unwrap_or(&lessons[0]);

        let (title, filename, hook) = lesson_to_index_entry(best_lesson);
        merged_lessons.push((title, filename, hook, merged));
    }

    // 5b. Score and select lessons within budget
    let budget_reserved_for_checkpoints = 30;
    let lesson_budget = config
        .budget
        .saturating_sub(budget_reserved_for_checkpoints);

    let scored_lessons: Vec<ScoredMemory> = merged_lessons
        .iter()
        .map(|(_, _, _, memory)| {
            let lesson = lessons.iter().find(|l| l.title == memory.name).cloned();
            let score = score_memory_relevance(memory, lesson.as_ref());
            ScoredMemory {
                memory: memory.clone(),
                lesson,
                score,
            }
        })
        .collect();

    let selected_lessons = select_memories_within_budget(scored_lessons, lesson_budget);

    report.actions.push(format!(
        "Budget filtering: {} lessons fit within {} line budget",
        selected_lessons.len(),
        lesson_budget
    ));

    // 5c. Write selected lesson memory files
    for scored in selected_lessons {
        // Find the corresponding title, filename, and hook from merged_lessons
        if let Some((title, filename, hook, _)) = merged_lessons
            .iter()
            .find(|(_, _, _, m)| m.name == scored.memory.name)
        {
            active_filenames.insert(filename.clone());

            if config.dry_run {
                report.actions.push(format!(
                    "Would write lesson: {} -> {}",
                    title,
                    memory_dir.join(filename).display()
                ));
            } else {
                write_memory_file(&memory_dir, &scored.memory)?;
                report.actions.push(format!(
                    "Wrote lesson: {} -> {}",
                    title,
                    memory_dir.join(filename).display()
                ));
            }

            index.add_entry(title, filename, hook);
            report.lessons_written += 1;
        }
    }

    // 5b. Write checkpoint memory files and update index
    for checkpoint in &checkpoints {
        let memory_file = checkpoint_to_memory(checkpoint);
        let (title, filename, hook) = checkpoint_to_index_entry(checkpoint);

        active_filenames.insert(filename.clone());

        if config.dry_run {
            report.actions.push(format!(
                "Would write checkpoint: {} -> {}",
                title,
                memory_dir.join(&filename).display()
            ));
        } else {
            write_memory_file(&memory_dir, &memory_file)?;
            report.actions.push(format!(
                "Wrote checkpoint: {} -> {}",
                title,
                memory_dir.join(&filename).display()
            ));
        }

        index.add_entry(&title, &filename, &hook);
        report.checkpoints_written += 1;
    }

    // 6. Clean up stale Nellie-managed entries
    let stale_filenames = find_stale_entries(&index, &active_filenames);

    for filename in &stale_filenames {
        let file_path = memory_dir.join(filename);

        if config.dry_run {
            report
                .actions
                .push(format!("Would remove stale: {}", file_path.display()));
        } else {
            // Remove the memory file if it exists
            if file_path.exists() {
                delete_memory_file(&file_path)?;
                report
                    .actions
                    .push(format!("Removed stale file: {}", file_path.display()));
            }
        }

        index.remove_entry(filename);
        report.stale_removed += 1;
    }

    // 7. Enforce 200-line budget
    index.enforce_line_limit(MAX_MEMORY_LINES);

    // 8. Save MEMORY.md (unless dry-run)
    if config.dry_run {
        report.actions.push(format!(
            "Would save MEMORY.md to {}",
            memory_md_path.display()
        ));
    } else {
        index.save(&memory_md_path)?;
        report
            .actions
            .push(format!("Saved MEMORY.md to {}", memory_md_path.display()));
    }

    report.index_entries = index.nellie_entry_count();
    report.index_lines = index.line_count();

    // 9. (Optional) Sync rules to ~/.claude/rules/
    if config.sync_rules {
        sync_rules_from_lessons(
            &lessons,
            config.dry_run,
            config.rules_dir_override.as_deref(),
            &mut report,
        )?;
    }

    Ok(report)
}

/// Syncs glob-conditioned rule files from lessons to `~/.claude/rules/`.
///
/// Only critical and warning severity lessons with at least one tag are
/// converted to rule files. Info-severity lessons are excluded (too
/// noisy for automatic context injection). Lessons without tags are
/// skipped because there are no meaningful globs to condition on.
///
/// After writing active rule files, stale Nellie-generated rule files
/// (from lessons that no longer qualify) are cleaned up.
///
/// # Arguments
///
/// * `lessons` - All lessons from the current sync (all severities).
/// * `dry_run` - If true, log what would be done without writing.
/// * `rules_dir_override` - Optional override for the rules directory
///   path. When `None`, resolves to `~/.claude/rules/`.
/// * `report` - Mutable report to record actions.
///
/// # Errors
///
/// Returns an error if the rules directory cannot be resolved or
/// file I/O operations fail.
fn sync_rules_from_lessons(
    lessons: &[LessonRecord],
    dry_run: bool,
    rules_dir_override: Option<&std::path::Path>,
    report: &mut SyncReport,
) -> crate::Result<()> {
    let rules_dir = match rules_dir_override {
        Some(dir) => dir.to_path_buf(),
        None => resolve_rules_dir()?,
    };
    report.rules_dir.clone_from(&rules_dir);

    let mapper = TagGlobMapper::new();

    // Collect IDs of lessons that qualify for rules
    let mut active_rule_ids: Vec<String> = Vec::new();

    for lesson in lessons {
        // Only critical and warning severity lessons become rules
        let sev = lesson.severity.to_lowercase();
        if sev != "critical" && sev != "warning" {
            continue;
        }

        // Skip lessons without tags (no meaningful globs)
        if lesson.tags.is_empty() {
            report
                .actions
                .push(format!("Skipped rule for '{}': no tags", lesson.title));
            continue;
        }

        let globs = mapper.tags_to_globs(&lesson.tags);
        if globs.is_empty() {
            report.actions.push(format!(
                "Skipped rule for '{}': tags produced no globs",
                lesson.title
            ));
            continue;
        }

        active_rule_ids.push(lesson.id.clone());

        if dry_run {
            report.actions.push(format!(
                "Would write rule: '{}' -> {} (globs: {:?})",
                lesson.title,
                rules_dir
                    .join(crate::claude_code::rules::rule_filename(&lesson.id))
                    .display(),
                globs,
            ));
        } else {
            let path = write_rule_file(&rules_dir, lesson, &globs)?;
            report.actions.push(format!(
                "Wrote rule: '{}' -> {}",
                lesson.title,
                path.display(),
            ));
        }

        report.rules_written += 1;
    }

    // Clean stale rules
    if dry_run {
        // In dry-run, count what would be cleaned but don't actually
        // delete. We still need to scan the directory to count.
        if rules_dir.exists() {
            let would_remove = count_stale_rules(&rules_dir, &active_rule_ids)?;
            if would_remove > 0 {
                report
                    .actions
                    .push(format!("Would remove {would_remove} stale rule file(s)"));
                report.rules_removed = would_remove;
            }
        }
    } else {
        let removed = clean_stale_rules(&rules_dir, &active_rule_ids)?;
        if removed > 0 {
            report
                .actions
                .push(format!("Removed {removed} stale rule file(s)"));
        }
        report.rules_removed = removed;
    }

    Ok(())
}

/// Counts the number of stale Nellie rule files without deleting them.
///
/// Used in dry-run mode to report how many files would be cleaned up.
fn count_stale_rules(dir: &std::path::Path, active_lesson_ids: &[String]) -> crate::Result<usize> {
    use crate::claude_code::rules::short_lesson_id;

    if !dir.exists() {
        return Ok(0);
    }

    let active_short_ids: HashSet<String> = active_lesson_ids
        .iter()
        .map(|id| short_lesson_id(id))
        .collect();

    let mut count = 0;
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.starts_with("nellie-") || !name_str.ends_with(".md") {
            continue;
        }

        let short_id = &name_str["nellie-".len()..name_str.len() - ".md".len()];
        if !active_short_ids.contains(short_id) {
            count += 1;
        }
    }

    Ok(count)
}

/// Queries lessons from the database ordered by priority.
///
/// Critical lessons come first, then warning, then info. Within each
/// severity level, lessons are ordered by creation time (newest first,
/// as returned by the database).
///
/// The total number of lessons returned is capped at `max_lessons`.
fn query_lessons_by_priority(
    db: &Database,
    max_lessons: usize,
) -> crate::Result<Vec<LessonRecord>> {
    let mut all_lessons: Vec<LessonRecord> = Vec::new();

    // Query by severity in priority order
    for severity in &["critical", "warning", "info"] {
        if all_lessons.len() >= max_lessons {
            break;
        }

        let lessons = db.with_conn(|conn| list_lessons_by_severity(conn, severity))?;
        all_lessons.extend(lessons);
    }

    // Truncate to max
    all_lessons.truncate(max_lessons);

    Ok(all_lessons)
}

/// Queries the latest checkpoints, limited to one per agent and capped
/// at `max_per_agent` total agents.
///
/// Uses [`list_distinct_agents`] to discover all agents, then fetches
/// the latest checkpoint for each. Results are returned sorted by
/// agent name.
fn query_latest_checkpoints(
    db: &Database,
    max_per_agent: usize,
) -> crate::Result<Vec<crate::storage::CheckpointRecord>> {
    let agents = db.with_conn(list_distinct_agents)?;

    let mut checkpoints = Vec::new();

    // Limit the number of agents we process
    let agents_to_process: Vec<_> = agents.into_iter().take(max_per_agent).collect();

    for agent in &agents_to_process {
        if let Some(cp) = db.with_conn(|conn| get_latest_checkpoint(conn, &agent.name))? {
            checkpoints.push(cp);
        }
    }

    // Sort by agent name for deterministic output
    checkpoints.sort_by(|a, b| a.agent.cmp(&b.agent));

    Ok(checkpoints)
}

/// Finds Nellie-managed filenames in the index that are not in the
/// active set.
///
/// These are entries that were previously synced but whose source
/// records no longer exist in the database (e.g., deleted lessons or
/// checkpoints from agents that are no longer tracked).
fn find_stale_entries(index: &MemoryIndex, active_filenames: &HashSet<String>) -> Vec<String> {
    index
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            MemoryEntry::Nellie { filename, .. } => {
                if active_filenames.contains(filename) {
                    None
                } else {
                    Some(filename.clone())
                }
            }
            MemoryEntry::Other(_) => None,
        })
        .collect()
}

/// Prints the sync report to stdout.
///
/// In dry-run mode, this prints the detailed action list. Otherwise,
/// it prints a concise summary. Rules info is included when
/// `rules_written` or `rules_removed` is non-zero.
pub fn print_report(report: &SyncReport, dry_run: bool) {
    if dry_run {
        println!("Dry run - no files written:");
        println!();
        for action in &report.actions {
            println!("  {action}");
        }
        println!();
    }

    println!("{report}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        init_storage, insert_checkpoint, insert_lesson, CheckpointRecord, LessonRecord,
    };
    use tempfile::TempDir;

    /// Helper: create a test database with schema initialized.
    fn test_db() -> (Database, TempDir) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("failed to open test db");
        init_storage(&db).expect("failed to init storage");
        (db, dir)
    }

    /// Helper: insert test lessons into the database.
    fn insert_test_lessons(db: &Database) {
        let critical = LessonRecord::new(
            "Never unwrap in prod",
            "Using unwrap() causes panics in production code",
            vec!["rust".into(), "error-handling".into()],
        )
        .with_severity("critical");

        let warning = LessonRecord::new(
            "Avoid blocking in async",
            "Blocking calls on the async runtime cause deadlocks",
            vec!["tokio".into(), "async".into()],
        )
        .with_severity("warning");

        let info = LessonRecord::new(
            "Use WAL mode for SQLite",
            "WAL mode allows concurrent readers with a single writer",
            vec!["sqlite".into()],
        )
        .with_severity("info");

        db.with_conn(|conn| insert_lesson(conn, &critical))
            .expect("insert critical");
        db.with_conn(|conn| insert_lesson(conn, &warning))
            .expect("insert warning");
        db.with_conn(|conn| insert_lesson(conn, &info))
            .expect("insert info");
    }

    /// Helper: insert test checkpoints into the database.
    fn insert_test_checkpoints(db: &Database) {
        let cp1 = CheckpointRecord::new(
            "mmn/nellie-rs",
            "Implementing deep hooks phase 1",
            serde_json::json!({
                "decisions": ["Use atomic writes", "Tag with [nellie]"],
                "next_steps": ["Implement sync command"]
            }),
        );

        let cp2 = CheckpointRecord::new(
            "mmn/other-project",
            "Debugging CI pipeline",
            serde_json::json!({
                "flags": ["blocked_on_ci"],
            }),
        );

        db.with_conn(|conn| insert_checkpoint(conn, &cp1))
            .expect("insert cp1");
        db.with_conn(|conn| insert_checkpoint(conn, &cp2))
            .expect("insert cp2");
    }

    // --- SyncConfig tests ---

    #[test]
    fn test_sync_config_defaults() {
        let config = SyncConfig::new(PathBuf::from("/tmp/project"));
        assert_eq!(config.project_dir, PathBuf::from("/tmp/project"));
        assert!(!config.dry_run);
        assert_eq!(config.max_lessons, DEFAULT_MAX_LESSONS);
        assert_eq!(config.max_checkpoints, DEFAULT_MAX_CHECKPOINTS);
        assert!(!config.sync_rules);
    }

    // --- query_lessons_by_priority tests ---

    #[test]
    fn test_query_lessons_empty_db() {
        let (db, _dir) = test_db();
        let lessons = query_lessons_by_priority(&db, 50).expect("query");
        assert!(lessons.is_empty());
    }

    #[test]
    fn test_query_lessons_priority_order() {
        let (db, _dir) = test_db();
        insert_test_lessons(&db);

        let lessons = query_lessons_by_priority(&db, 50).expect("query");
        assert_eq!(lessons.len(), 3);

        // Critical should come first
        assert_eq!(lessons[0].severity, "critical");
        // Then warning
        assert_eq!(lessons[1].severity, "warning");
        // Then info
        assert_eq!(lessons[2].severity, "info");
    }

    #[test]
    fn test_query_lessons_respects_max() {
        let (db, _dir) = test_db();
        insert_test_lessons(&db);

        let lessons = query_lessons_by_priority(&db, 2).expect("query");
        assert_eq!(lessons.len(), 2);

        // Should get critical and warning (highest priority)
        assert_eq!(lessons[0].severity, "critical");
        assert_eq!(lessons[1].severity, "warning");
    }

    #[test]
    fn test_query_lessons_max_one() {
        let (db, _dir) = test_db();
        insert_test_lessons(&db);

        let lessons = query_lessons_by_priority(&db, 1).expect("query");
        assert_eq!(lessons.len(), 1);
        assert_eq!(lessons[0].severity, "critical");
    }

    // --- query_latest_checkpoints tests ---

    #[test]
    fn test_query_checkpoints_empty_db() {
        let (db, _dir) = test_db();
        let checkpoints = query_latest_checkpoints(&db, 10).expect("query");
        assert!(checkpoints.is_empty());
    }

    #[test]
    fn test_query_checkpoints_returns_latest_per_agent() {
        let (db, _dir) = test_db();
        insert_test_checkpoints(&db);

        let checkpoints = query_latest_checkpoints(&db, 10).expect("query");
        assert_eq!(checkpoints.len(), 2);

        // Should be sorted by agent name
        assert_eq!(checkpoints[0].agent, "mmn/nellie-rs");
        assert_eq!(checkpoints[1].agent, "mmn/other-project");
    }

    #[test]
    fn test_query_checkpoints_respects_max() {
        let (db, _dir) = test_db();
        insert_test_checkpoints(&db);

        let checkpoints = query_latest_checkpoints(&db, 1).expect("query");
        assert_eq!(checkpoints.len(), 1);
    }

    // --- find_stale_entries tests ---

    #[test]
    fn test_find_stale_entries_none() {
        let mut index = MemoryIndex::new();
        index.add_entry("Lesson A", "lesson_a.md", "hook a");
        index.add_entry("Lesson B", "lesson_b.md", "hook b");

        let active: HashSet<String> = ["lesson_a.md".to_string(), "lesson_b.md".to_string()]
            .into_iter()
            .collect();

        let stale = find_stale_entries(&index, &active);
        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_entries_some() {
        let mut index = MemoryIndex::new();
        index.add_entry("Lesson A", "lesson_a.md", "hook a");
        index.add_entry("Lesson B", "lesson_b.md", "hook b");
        index.add_entry("Lesson C", "lesson_c.md", "hook c");

        let active: HashSet<String> = ["lesson_a.md".to_string()].into_iter().collect();

        let stale = find_stale_entries(&index, &active);
        assert_eq!(stale.len(), 2);
        assert!(stale.contains(&"lesson_b.md".to_string()));
        assert!(stale.contains(&"lesson_c.md".to_string()));
    }

    #[test]
    fn test_find_stale_entries_ignores_non_nellie() {
        // Create a temp file with mixed Nellie and non-Nellie entries
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("MEMORY.md");
        std::fs::write(
            &path,
            "# Header\n- [Manual](manual.md) -- some hook\n- [Lesson A](lesson_a.md) -- hook a [nellie]\n",
        )
        .expect("write");

        let loaded = MemoryIndex::load(&path).expect("load");

        let active: HashSet<String> = ["lesson_a.md".to_string()].into_iter().collect();

        let stale = find_stale_entries(&loaded, &active);
        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_entries_all_stale() {
        let mut index = MemoryIndex::new();
        index.add_entry("Old Lesson", "old_lesson.md", "old hook");

        let active: HashSet<String> = HashSet::new();

        let stale = find_stale_entries(&index, &active);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], "old_lesson.md");
    }

    // --- SyncReport tests ---

    #[test]
    fn test_sync_report_display() {
        let report = SyncReport {
            lessons_written: 3,
            checkpoints_written: 2,
            stale_removed: 1,
            index_entries: 5,
            index_lines: 10,
            memory_dir: PathBuf::from("/tmp/memory"),
            rules_written: 0,
            rules_removed: 0,
            rules_dir: PathBuf::new(),
            actions: vec![],
        };

        let display = format!("{report}");
        assert!(display.contains("Lessons written:     3"));
        assert!(display.contains("Checkpoints written: 2"));
        assert!(display.contains("Stale entries removed: 1"));
        // Rules info should NOT appear when both are 0
        assert!(!display.contains("Rules"));
    }

    #[test]
    fn test_sync_report_display_with_rules() {
        let report = SyncReport {
            lessons_written: 3,
            checkpoints_written: 0,
            stale_removed: 0,
            index_entries: 3,
            index_lines: 6,
            memory_dir: PathBuf::from("/tmp/memory"),
            rules_written: 2,
            rules_removed: 1,
            rules_dir: PathBuf::from("/tmp/rules"),
            actions: vec![],
        };

        let display = format!("{report}");
        assert!(display.contains("Rules written:       2"));
        assert!(display.contains("Rules removed:       1"));
        assert!(display.contains("Rules dir:"));
    }

    // --- Full integration tests ---

    #[test]
    fn test_execute_sync_empty_db() {
        let (db, _db_dir) = test_db();
        let project_dir = TempDir::new().expect("temp dir");

        let config = SyncConfig::new(project_dir.path().to_path_buf());
        let report = execute_sync(&db, &config).expect("sync");

        assert_eq!(report.lessons_written, 0);
        assert_eq!(report.checkpoints_written, 0);
        assert_eq!(report.stale_removed, 0);
    }

    #[test]
    fn test_execute_sync_with_lessons() {
        let (db, _db_dir) = test_db();
        insert_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let config = SyncConfig::new(project_dir.path().to_path_buf());
        let report = execute_sync(&db, &config).expect("sync");

        assert_eq!(report.lessons_written, 3);
        assert_eq!(report.checkpoints_written, 0);
        assert_eq!(report.stale_removed, 0);

        // Verify memory files were created
        let memory_dir = &report.memory_dir;
        assert!(memory_dir.join("MEMORY.md").exists());
        assert!(memory_dir.join("never_unwrap_in_prod.md").exists());
        assert!(memory_dir.join("avoid_blocking_in_async.md").exists());
        assert!(memory_dir.join("use_wal_mode_for_sqlite.md").exists());
    }

    #[test]
    fn test_execute_sync_with_checkpoints() {
        let (db, _db_dir) = test_db();
        insert_test_checkpoints(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let config = SyncConfig::new(project_dir.path().to_path_buf());
        let report = execute_sync(&db, &config).expect("sync");

        assert_eq!(report.lessons_written, 0);
        assert_eq!(report.checkpoints_written, 2);
    }

    #[test]
    fn test_execute_sync_with_lessons_and_checkpoints() {
        let (db, _db_dir) = test_db();
        insert_test_lessons(&db);
        insert_test_checkpoints(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let config = SyncConfig::new(project_dir.path().to_path_buf());
        let report = execute_sync(&db, &config).expect("sync");

        assert_eq!(report.lessons_written, 3);
        assert_eq!(report.checkpoints_written, 2);
        assert!(report.index_entries >= 5);
    }

    #[test]
    fn test_execute_sync_dry_run() {
        let (db, _db_dir) = test_db();
        insert_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.dry_run = true;

        let report = execute_sync(&db, &config).expect("sync");

        assert_eq!(report.lessons_written, 3);

        // In dry-run mode, no files should actually be written
        let memory_dir = &report.memory_dir;
        assert!(!memory_dir.join("never_unwrap_in_prod.md").exists());

        // But actions should describe what would happen
        let has_would_write = report.actions.iter().any(|a| a.contains("Would write"));
        assert!(has_would_write, "dry-run should have 'Would write' actions");
    }

    #[test]
    fn test_execute_sync_max_lessons() {
        let (db, _db_dir) = test_db();
        insert_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.max_lessons = 1;

        let report = execute_sync(&db, &config).expect("sync");

        // Should only sync 1 lesson (the critical one)
        assert_eq!(report.lessons_written, 1);
    }

    #[test]
    fn test_execute_sync_stale_cleanup() {
        let (db, _db_dir) = test_db();
        insert_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let config = SyncConfig::new(project_dir.path().to_path_buf());

        // First sync: creates 3 lesson files
        let report1 = execute_sync(&db, &config).expect("first sync");
        assert_eq!(report1.lessons_written, 3);
        assert_eq!(report1.stale_removed, 0);

        let memory_dir = &report1.memory_dir;

        // Delete two lessons from the DB so they become stale
        let lessons = db
            .with_conn(|conn| list_lessons_by_severity(conn, "warning"))
            .expect("query");
        for lesson in &lessons {
            db.with_conn(|conn| crate::storage::delete_lesson(conn, &lesson.id))
                .expect("delete");
        }
        let lessons = db
            .with_conn(|conn| list_lessons_by_severity(conn, "info"))
            .expect("query");
        for lesson in &lessons {
            db.with_conn(|conn| crate::storage::delete_lesson(conn, &lesson.id))
                .expect("delete");
        }

        // Second sync: should clean up stale entries
        let report2 = execute_sync(&db, &config).expect("second sync");
        assert_eq!(report2.lessons_written, 1); // only the critical one remains
        assert_eq!(report2.stale_removed, 2); // the two deleted lessons

        // Verify stale files were removed
        assert!(!memory_dir.join("avoid_blocking_in_async.md").exists());
        assert!(!memory_dir.join("use_wal_mode_for_sqlite.md").exists());

        // Verify the surviving file still exists
        assert!(memory_dir.join("never_unwrap_in_prod.md").exists());
    }

    #[test]
    fn test_execute_sync_preserves_non_nellie_entries() {
        let (db, _db_dir) = test_db();
        insert_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let config = SyncConfig::new(project_dir.path().to_path_buf());

        // First, create a MEMORY.md with a manual entry
        let memory_dir = resolve_project_memory_dir(project_dir.path()).expect("resolve");
        std::fs::create_dir_all(&memory_dir).expect("mkdir");
        std::fs::write(
            memory_dir.join("MEMORY.md"),
            "# Project Memory\n\n- [Manual Entry](manual.md) -- hand-written note\n",
        )
        .expect("write");

        // Sync should add lesson entries but keep the manual one
        let report = execute_sync(&db, &config).expect("sync");
        assert_eq!(report.lessons_written, 3);

        // Read the MEMORY.md and verify manual entry is preserved
        let content = std::fs::read_to_string(memory_dir.join("MEMORY.md")).expect("read");
        assert!(
            content.contains("Manual Entry"),
            "Manual entry should be preserved"
        );
        assert!(
            content.contains("[nellie]"),
            "Nellie entries should have [nellie] tag"
        );
    }

    #[test]
    fn test_execute_sync_idempotent() {
        let (db, _db_dir) = test_db();
        insert_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let config = SyncConfig::new(project_dir.path().to_path_buf());

        // Run sync twice
        let report1 = execute_sync(&db, &config).expect("first sync");
        let report2 = execute_sync(&db, &config).expect("second sync");

        // Both should write the same number of lessons
        assert_eq!(report1.lessons_written, report2.lessons_written);

        // Second sync should have no stale removals
        assert_eq!(report2.stale_removed, 0);

        // Index counts should be the same
        assert_eq!(report1.index_entries, report2.index_entries);
    }

    #[test]
    fn test_execute_sync_max_checkpoints() {
        let (db, _db_dir) = test_db();
        insert_test_checkpoints(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.max_checkpoints = 1;

        let report = execute_sync(&db, &config).expect("sync");

        // Should only sync 1 checkpoint (from the first agent)
        assert_eq!(report.checkpoints_written, 1);
    }

    // --- Rules sync tests ---

    /// Helper: insert test lessons with varying severity and tags
    /// for rules sync testing. Returns IDs for easy reference.
    fn insert_rules_test_lessons(db: &Database) -> Vec<String> {
        let critical_with_tags = LessonRecord::new(
            "SQLite WAL lock contention",
            "Use WAL2 mode to avoid lock contention",
            vec!["sqlite".into(), "database".into()],
        )
        .with_severity("critical");

        let warning_with_tags = LessonRecord::new(
            "Avoid unwrap in async handlers",
            "Unwrap in async code causes task panics",
            vec!["rust".into(), "tokio".into()],
        )
        .with_severity("warning");

        let info_with_tags = LessonRecord::new(
            "Prefer tracing over println",
            "Use tracing crate for structured logging",
            vec!["rust".into(), "logging".into()],
        )
        .with_severity("info");

        let critical_no_tags = LessonRecord::new(
            "Always test edge cases",
            "Edge cases cause the most production bugs",
            vec![],
        )
        .with_severity("critical");

        let ids = vec![
            critical_with_tags.id.clone(),
            warning_with_tags.id.clone(),
            info_with_tags.id.clone(),
            critical_no_tags.id.clone(),
        ];

        db.with_conn(|conn| insert_lesson(conn, &critical_with_tags))
            .expect("insert");
        db.with_conn(|conn| insert_lesson(conn, &warning_with_tags))
            .expect("insert");
        db.with_conn(|conn| insert_lesson(conn, &info_with_tags))
            .expect("insert");
        db.with_conn(|conn| insert_lesson(conn, &critical_no_tags))
            .expect("insert");

        ids
    }

    #[test]
    fn test_sync_rules_from_lessons_filtering_logic() {
        let (db, _db_dir) = test_db();
        let ids = insert_rules_test_lessons(&db);

        // Test the filtering logic: which lessons qualify for rules?
        let lessons = query_lessons_by_priority(&db, 50).expect("query");
        assert_eq!(lessons.len(), 4);

        // Check which ones qualify for rules:
        // - critical with tags: YES
        // - warning with tags: YES
        // - info with tags: NO (info severity)
        // - critical no tags: NO (no tags)
        let qualifying: Vec<_> = lessons
            .iter()
            .filter(|l| {
                let sev = l.severity.to_lowercase();
                (sev == "critical" || sev == "warning") && !l.tags.is_empty()
            })
            .collect();
        assert_eq!(qualifying.len(), 2);
        assert_eq!(qualifying[0].severity, "critical");
        assert_eq!(qualifying[1].severity, "warning");

        // Verify the IDs are what we expect
        assert_eq!(qualifying[0].id, ids[0]); // critical with tags
        assert_eq!(qualifying[1].id, ids[1]); // warning with tags
    }

    #[test]
    fn test_sync_rules_from_lessons_direct() {
        let (db, _db_dir) = test_db();
        let _ids = insert_rules_test_lessons(&db);
        let rules_dir = TempDir::new().expect("rules temp dir");

        let lessons = query_lessons_by_priority(&db, 50).expect("query");
        let mut report = SyncReport::default();

        sync_rules_from_lessons(&lessons, false, Some(rules_dir.path()), &mut report)
            .expect("sync rules");

        assert_eq!(report.rules_written, 2);
        assert_eq!(report.rules_removed, 0);

        // Verify files on disk
        let rule_files: Vec<_> = std::fs::read_dir(rules_dir.path())
            .expect("read rules dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name_str = name.to_string_lossy();
                name_str.starts_with("nellie-") && name_str.ends_with(".md")
            })
            .collect();
        assert_eq!(rule_files.len(), 2);
    }

    #[test]
    fn test_execute_sync_with_rules_flag() {
        let (db, _db_dir) = test_db();
        let _ids = insert_rules_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let rules_dir = TempDir::new().expect("rules temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.sync_rules = true;
        config.rules_dir_override = Some(rules_dir.path().to_path_buf());

        let report = execute_sync(&db, &config).expect("sync");

        // 4 lessons total written as memory files
        assert_eq!(report.lessons_written, 4);

        // 2 rules written (critical+tags and warning+tags)
        assert_eq!(report.rules_written, 2);

        // No stale rules to remove on first sync
        assert_eq!(report.rules_removed, 0);

        // Rules dir should be set
        assert!(
            !report.rules_dir.as_os_str().is_empty(),
            "rules_dir should be set"
        );

        // Verify rule files exist on disk
        let rule_files: Vec<_> = std::fs::read_dir(rules_dir.path())
            .expect("read rules dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name_str = name.to_string_lossy();
                name_str.starts_with("nellie-") && name_str.ends_with(".md")
            })
            .collect();

        assert_eq!(
            rule_files.len(),
            2,
            "Expected 2 rule files, found: {:?}",
            rule_files.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );

        // Verify rule file content has proper frontmatter
        for entry in &rule_files {
            let content = std::fs::read_to_string(entry.path()).expect("read rule");
            assert!(
                content.starts_with("---\n"),
                "Rule should start with frontmatter"
            );
            assert!(content.contains("globs:"), "Rule should have globs field");
            assert!(
                content.contains("---\n\n##"),
                "Rule should have title after frontmatter"
            );
        }
    }

    #[test]
    fn test_execute_sync_rules_without_flag_does_nothing() {
        let (db, _db_dir) = test_db();
        let _ids = insert_rules_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let config = SyncConfig::new(project_dir.path().to_path_buf());
        // sync_rules defaults to false

        let report = execute_sync(&db, &config).expect("sync");

        assert_eq!(report.rules_written, 0);
        assert_eq!(report.rules_removed, 0);
        assert!(
            report.rules_dir.as_os_str().is_empty(),
            "rules_dir should be empty when rules not synced"
        );
    }

    #[test]
    fn test_execute_sync_rules_dry_run() {
        let (db, _db_dir) = test_db();
        let _ids = insert_rules_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let rules_dir = TempDir::new().expect("rules temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.sync_rules = true;
        config.dry_run = true;
        config.rules_dir_override = Some(rules_dir.path().to_path_buf());

        let report = execute_sync(&db, &config).expect("sync");

        // Rules should be counted but not written
        assert_eq!(report.rules_written, 2);

        // Verify no rule files exist on disk
        let rule_files: Vec<_> = std::fs::read_dir(rules_dir.path())
            .expect("read rules dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name_str = name.to_string_lossy();
                name_str.starts_with("nellie-") && name_str.ends_with(".md")
            })
            .collect();
        assert_eq!(rule_files.len(), 0, "Dry-run should not create files");

        // Actions should contain "Would write rule"
        let has_would_write = report
            .actions
            .iter()
            .any(|a| a.contains("Would write rule"));
        assert!(
            has_would_write,
            "dry-run should have 'Would write rule' actions"
        );
    }

    #[test]
    fn test_execute_sync_rules_stale_cleanup() {
        let (db, _db_dir) = test_db();
        let ids = insert_rules_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let rules_dir = TempDir::new().expect("rules temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.sync_rules = true;
        config.rules_dir_override = Some(rules_dir.path().to_path_buf());

        // First sync: creates rule files
        let report1 = execute_sync(&db, &config).expect("first sync");
        assert_eq!(report1.rules_written, 2);
        assert_eq!(report1.rules_removed, 0);

        // Delete the warning lesson so its rule becomes stale
        db.with_conn(|conn| crate::storage::delete_lesson(conn, &ids[1]))
            .expect("delete warning lesson");

        // Second sync: should clean up the stale rule
        let report2 = execute_sync(&db, &config).expect("second sync");
        assert_eq!(
            report2.rules_written, 1,
            "Only the critical lesson rule should remain"
        );
        assert_eq!(
            report2.rules_removed, 1,
            "The warning lesson rule should be removed"
        );

        // Verify only 1 rule file remains
        let remaining: Vec<_> = std::fs::read_dir(rules_dir.path())
            .expect("read rules dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name_str = name.to_string_lossy();
                name_str.starts_with("nellie-") && name_str.ends_with(".md")
            })
            .collect();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_execute_sync_rules_info_excluded() {
        let (db, _db_dir) = test_db();

        // Insert only info-severity lessons with tags
        let info = LessonRecord::new(
            "Info lesson with tags",
            "This is an info lesson",
            vec!["rust".into()],
        )
        .with_severity("info");
        db.with_conn(|conn| insert_lesson(conn, &info))
            .expect("insert");

        let project_dir = TempDir::new().expect("temp dir");
        let rules_dir = TempDir::new().expect("rules temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.sync_rules = true;
        config.rules_dir_override = Some(rules_dir.path().to_path_buf());

        let report = execute_sync(&db, &config).expect("sync");

        // Lesson should be written as memory file
        assert_eq!(report.lessons_written, 1);

        // But NOT as a rule (info severity excluded)
        assert_eq!(report.rules_written, 0);
    }

    #[test]
    fn test_execute_sync_rules_no_tags_skipped() {
        let (db, _db_dir) = test_db();

        // Insert a critical lesson with no tags
        let critical = LessonRecord::new(
            "Critical no tags",
            "This is critical but has no tags",
            vec![],
        )
        .with_severity("critical");
        db.with_conn(|conn| insert_lesson(conn, &critical))
            .expect("insert");

        let project_dir = TempDir::new().expect("temp dir");
        let rules_dir = TempDir::new().expect("rules temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.sync_rules = true;
        config.rules_dir_override = Some(rules_dir.path().to_path_buf());

        let report = execute_sync(&db, &config).expect("sync");

        assert_eq!(report.lessons_written, 1);
        assert_eq!(report.rules_written, 0);

        // Verify "Skipped rule" action is present
        let has_skipped = report.actions.iter().any(|a| a.contains("Skipped rule"));
        assert!(has_skipped, "Should log skipped rule for no-tags lesson");
    }

    #[test]
    fn test_execute_sync_rules_idempotent() {
        let (db, _db_dir) = test_db();
        let _ids = insert_rules_test_lessons(&db);

        let project_dir = TempDir::new().expect("temp dir");
        let rules_dir = TempDir::new().expect("rules temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.sync_rules = true;
        config.rules_dir_override = Some(rules_dir.path().to_path_buf());

        // Sync twice
        let report1 = execute_sync(&db, &config).expect("first sync");
        let report2 = execute_sync(&db, &config).expect("second sync");

        assert_eq!(report1.rules_written, report2.rules_written);
        assert_eq!(report2.rules_removed, 0, "No stale rules on re-sync");
    }

    #[test]
    fn test_execute_sync_rules_correct_globs() {
        let (db, _db_dir) = test_db();

        // Insert a critical lesson with sqlite tags
        let lesson = LessonRecord::new(
            "SQLite WAL gotcha",
            "Always use WAL mode for SQLite",
            vec!["sqlite".into(), "database".into()],
        )
        .with_severity("critical");

        let lesson_id = lesson.id.clone();

        db.with_conn(|conn| insert_lesson(conn, &lesson))
            .expect("insert");

        let project_dir = TempDir::new().expect("temp dir");
        let rules_dir = TempDir::new().expect("rules temp dir");
        let mut config = SyncConfig::new(project_dir.path().to_path_buf());
        config.sync_rules = true;
        config.rules_dir_override = Some(rules_dir.path().to_path_buf());

        let report = execute_sync(&db, &config).expect("sync");
        assert_eq!(report.rules_written, 1);

        // Read the rule file and verify its globs
        let rule_file = report
            .rules_dir
            .join(crate::claude_code::rules::rule_filename(&lesson_id));
        let content = std::fs::read_to_string(&rule_file).expect("read rule");

        // sqlite and database tags should produce storage globs
        assert!(
            content.contains("src/storage/**/*.rs"),
            "Should contain storage glob"
        );
        assert!(
            content.contains("**/*sqlite*"),
            "Should contain sqlite glob"
        );
        assert!(
            content.contains("[critical]"),
            "Should contain severity tag"
        );
        assert!(
            content.contains("SQLite WAL gotcha"),
            "Should contain lesson title"
        );
    }

    #[test]
    fn test_count_stale_rules() {
        let dir = TempDir::new().expect("temp dir");
        let rules_path = dir.path();

        // Create some Nellie rule files
        std::fs::write(rules_path.join("nellie-aabbccdd.md"), "rule 1").expect("write");
        std::fs::write(rules_path.join("nellie-11223344.md"), "rule 2").expect("write");
        std::fs::write(rules_path.join("nellie-55667788.md"), "rule 3").expect("write");
        // Non-Nellie file should be ignored
        std::fs::write(rules_path.join("custom-rule.md"), "custom").expect("write");

        // Only one is active
        let active = vec!["lesson_aabbccddee".to_string()]; // short: aabbccdd

        let count = count_stale_rules(rules_path, &active).expect("count");
        assert_eq!(count, 2, "2 of 3 Nellie rules should be stale");
    }

    #[test]
    fn test_count_stale_rules_nonexistent_dir() {
        let count =
            count_stale_rules(std::path::Path::new("/nonexistent/path"), &[]).expect("count");
        assert_eq!(count, 0);
    }
}
