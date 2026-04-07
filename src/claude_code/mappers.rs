//! Mappers that convert Nellie records into Claude Code memory files.
//!
//! This module bridges Nellie's internal data model
//! ([`LessonRecord`], [`CheckpointRecord`]) with Claude Code's native
//! memory system ([`MemoryFile`], [`MemoryIndex`]).
//!
//! # Lesson Mapping
//!
//! - [`lesson_to_memory`]: Converts a lesson into a full
//!   [`MemoryFile`] with proper frontmatter type based on severity.
//! - [`lesson_to_index_entry`]: Produces the (title, filename, hook)
//!   triple needed by [`MemoryIndex::add_entry`].
//! - [`lesson_memory_filename`]: Derives a safe filename from a
//!   lesson title, delegating to
//!   [`memory_writer::memory_filename`](super::memory_writer::memory_filename).
//!
//! # Checkpoint Mapping
//!
//! - [`checkpoint_to_memory`]: Converts a checkpoint into a
//!   [`MemoryFile`] with `Project` type. Extracts `decisions`,
//!   `flags`, and `next_steps` from the checkpoint's `state` JSON.
//! - [`checkpoint_to_index_entry`]: Produces the (title, filename,
//!   hook) triple for a checkpoint's MEMORY.md index entry.
//! - [`filter_latest_checkpoints`]: Given a list of checkpoints,
//!   returns only the most recent per agent.
//!
//! # Severity-to-Type Mapping
//!
//! | Severity   | MemoryType  | Rationale                               |
//! |------------|-------------|------------------------------------------|
//! | `critical` | `Feedback`  | Critical corrections should surface fast |
//! | `warning`  | `Feedback`  | Warnings are corrective in nature        |
//! | `info`     | `Project`   | General knowledge for the project        |

use std::collections::HashMap;

use crate::claude_code::memory_writer::{memory_filename, MemoryFile, MemoryType};
use crate::storage::{CheckpointRecord, LessonRecord};

/// Maximum length (in characters) for the `description` field of a
/// generated [`MemoryFile`]. Claude Code's frontmatter description
/// should stay concise.
const MAX_DESCRIPTION_LENGTH: usize = 100;

/// Maps a Nellie lesson severity to a Claude Code [`MemoryType`].
///
/// - `"critical"` and `"warning"` map to [`MemoryType::Feedback`]
///   because they represent corrective knowledge.
/// - Everything else (including `"info"`) maps to
///   [`MemoryType::Project`] as general project knowledge.
fn severity_to_memory_type(severity: &str) -> MemoryType {
    match severity.to_lowercase().as_str() {
        "critical" | "warning" => MemoryType::Feedback,
        _ => MemoryType::Project,
    }
}

/// Converts a [`LessonRecord`] into a Claude Code [`MemoryFile`].
///
/// The mapping is:
/// - **name**: the lesson's `title`
/// - **description**: the first 100 characters of `content`
///   (truncated at a word boundary when possible)
/// - **type**: determined by [`severity_to_memory_type`]
/// - **content**: the full lesson content, prefixed with a severity
///   tag (e.g., `**[CRITICAL]**`) and any tags from the lesson
///
/// # Examples
///
/// ```rust,ignore
/// use nellie::storage::models::LessonRecord;
/// use nellie::claude_code::mappers::lesson_to_memory;
///
/// let lesson = LessonRecord::new(
///     "Never use unwrap in prod",
///     "Using unwrap() in production code can cause panics.",
///     vec!["rust".into(), "error-handling".into()],
/// ).with_severity("critical");
///
/// let mf = lesson_to_memory(&lesson);
/// assert_eq!(mf.name, "Never use unwrap in prod");
/// assert_eq!(mf.memory_type, MemoryType::Feedback);
/// ```
pub fn lesson_to_memory(lesson: &LessonRecord) -> MemoryFile {
    let name = lesson.title.clone();
    let description = truncate_description(&lesson.content);
    let memory_type = severity_to_memory_type(&lesson.severity);
    let content = format_lesson_content(lesson);

    MemoryFile::new(name, description, memory_type, content)
}

/// Returns the (title, filename, hook) triple for a MEMORY.md index
/// entry derived from a lesson.
///
/// - **title**: the lesson's title
/// - **filename**: safe filename via [`lesson_memory_filename`]
/// - **hook**: concise one-line summary combining severity and a
///   content excerpt
///
/// The returned values are suitable for passing directly to
/// [`MemoryIndex::add_entry`](super::memory_index::MemoryIndex::add_entry).
pub fn lesson_to_index_entry(lesson: &LessonRecord) -> (String, String, String) {
    let title = lesson.title.clone();
    let filename = lesson_memory_filename(&lesson.title);
    let hook = format_lesson_hook(lesson);
    (title, filename, hook)
}

/// Derives a safe filename from a lesson title.
///
/// This delegates to
/// [`memory_writer::memory_filename`](super::memory_writer::memory_filename)
/// to ensure consistent filename generation across the codebase.
///
/// # Examples
///
/// ```rust,ignore
/// use nellie::claude_code::mappers::lesson_memory_filename;
///
/// assert_eq!(
///     lesson_memory_filename("SQLite WAL Mode"),
///     "sqlite_wal_mode.md"
/// );
/// ```
pub fn lesson_memory_filename(title: &str) -> String {
    memory_filename(title)
}

/// Truncates content to at most [`MAX_DESCRIPTION_LENGTH`] characters
/// for use as a frontmatter description.
///
/// When truncation is needed, the function tries to break at a word
/// boundary (space) to avoid mid-word cuts, then appends "...".
fn truncate_description(content: &str) -> String {
    // Take the first line only (descriptions should be one-line)
    let first_line = content.lines().next().unwrap_or("");

    // Strip any leading markdown formatting
    let cleaned = first_line
        .trim_start_matches('#')
        .trim_start_matches('*')
        .trim_start_matches('-')
        .trim();

    if cleaned.len() <= MAX_DESCRIPTION_LENGTH {
        return cleaned.to_string();
    }

    // Find the last space before the limit to break at a word boundary
    let limit = MAX_DESCRIPTION_LENGTH - 3; // room for "..."
    let truncated = &cleaned[..limit];
    truncated.rfind(' ').map_or_else(
        || format!("{truncated}..."),
        |last_space| format!("{}...", &cleaned[..last_space]),
    )
}

/// Formats the full content body for a lesson memory file.
///
/// The body includes:
/// 1. A severity tag line (e.g., `**[CRITICAL]**`)
/// 2. Tags as a comma-separated list (if any)
/// 3. The full lesson content
fn format_lesson_content(lesson: &LessonRecord) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Severity tag
    let severity_upper = lesson.severity.to_uppercase();
    parts.push(format!("**[{severity_upper}]**"));

    // Tags
    if !lesson.tags.is_empty() {
        let tag_line = lesson
            .tags
            .iter()
            .map(|t| format!("`{t}`"))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("Tags: {tag_line}"));
    }

    // Blank line before content
    parts.push(String::new());

    // Full content
    parts.push(lesson.content.clone());

    parts.join("\n")
}

/// Formats a concise hook string for a MEMORY.md index entry.
///
/// The hook combines the severity (if not "info") with a truncated
/// excerpt of the content. The result is kept short enough to fit
/// within the 150-character line limit of MEMORY.md entries (the
/// [`MemoryIndex`](super::memory_index::MemoryIndex) handles the
/// final truncation).
fn format_lesson_hook(lesson: &LessonRecord) -> String {
    // Build a prefix from severity (skip for "info" since it's default)
    let prefix = match lesson.severity.to_lowercase().as_str() {
        "critical" => "[critical] ",
        "warning" => "[warning] ",
        _ => "",
    };

    // Use truncated content as the hook body
    let excerpt = truncate_to_words(&lesson.content, 80 - prefix.len());

    format!("{prefix}{excerpt}")
}

/// Truncates text to at most `max_chars` characters, breaking at a
/// word boundary when possible.
fn truncate_to_words(text: &str, max_chars: usize) -> String {
    // Take first line only
    let first_line = text.lines().next().unwrap_or("");
    let trimmed = first_line.trim();

    if trimmed.len() <= max_chars {
        return trimmed.to_string();
    }

    let limit = max_chars.saturating_sub(3);
    // Find the nearest char boundary at or before `limit` to avoid panicking on multi-byte UTF-8
    let safe_limit = trimmed
        .char_indices()
        .take_while(|(i, _)| *i <= limit)
        .last()
        .map_or(0, |(i, _)| i);
    let truncated = &trimmed[..safe_limit];
    truncated.rfind(' ').map_or_else(
        || format!("{truncated}..."),
        |last_space| format!("{}...", &trimmed[..last_space]),
    )
}

// ================================================================
// Checkpoint Mapping
// ================================================================

/// Converts a [`CheckpointRecord`] into a Claude Code [`MemoryFile`].
///
/// The mapping is:
/// - **name**: `"checkpoint_{agent}_{date}"` where date is
///   `YYYY-MM-DD` derived from `created_at`
/// - **description**: the checkpoint's `working_on` field, truncated
///   to 100 characters
/// - **type**: always [`MemoryType::Project`] — checkpoints are
///   project context
/// - **content**: the `working_on` summary plus key fields extracted
///   from the `state` JSON (`decisions`, `flags`, `next_steps`),
///   formatted as Markdown
///
/// # State JSON Fields
///
/// The checkpoint's `state` field is a JSON value that may contain:
/// - `decisions`: array of strings or object with string values
/// - `flags`: array of strings or object with string values
/// - `next_steps`: array of strings or object with string values
///
/// All three are optional. Missing or non-array/object fields are
/// silently skipped.
///
/// # Examples
///
/// ```rust,ignore
/// use nellie::storage::CheckpointRecord;
/// use nellie::claude_code::mappers::checkpoint_to_memory;
///
/// let cp = CheckpointRecord::new(
///     "user/my-project",
///     "Implementing deep hooks phase 1",
///     serde_json::json!({
///         "decisions": ["Use atomic writes", "Tag with [nellie]"],
///         "flags": ["blocked_on_ci"],
///         "next_steps": ["Implement sync command"]
///     }),
/// );
///
/// let mf = checkpoint_to_memory(&cp);
/// assert!(mf.name.starts_with("checkpoint_user_my-project_"));
/// assert_eq!(mf.memory_type, MemoryType::Project);
/// ```
pub fn checkpoint_to_memory(checkpoint: &CheckpointRecord) -> MemoryFile {
    let name = checkpoint_memory_name(checkpoint);
    let description = truncate_description(&checkpoint.working_on);
    let content = format_checkpoint_content(checkpoint);

    MemoryFile::new(name, description, MemoryType::Project, content)
}

/// Returns the (title, filename, hook) triple for a MEMORY.md index
/// entry derived from a checkpoint.
///
/// - **title**: the checkpoint name (same as the memory file name)
/// - **filename**: safe filename via [`memory_filename`]
/// - **hook**: concise `working_on` excerpt
///
/// The returned values are suitable for passing directly to
/// [`MemoryIndex::add_entry`](super::memory_index::MemoryIndex::add_entry).
pub fn checkpoint_to_index_entry(checkpoint: &CheckpointRecord) -> (String, String, String) {
    let title = checkpoint_memory_name(checkpoint);
    let filename = memory_filename(&title);
    let hook = format_checkpoint_hook(checkpoint);
    (title, filename, hook)
}

/// Filters a slice of checkpoints to keep only the most recent one
/// per agent.
///
/// When multiple checkpoints exist for the same agent, only the one
/// with the highest `created_at` timestamp is retained. The returned
/// vector is sorted by agent name for deterministic output.
///
/// # Examples
///
/// ```rust,ignore
/// use nellie::storage::CheckpointRecord;
/// use nellie::claude_code::mappers::filter_latest_checkpoints;
///
/// let cps = vec![
///     CheckpointRecord::new("agent-a", "old work", json!({})),
///     CheckpointRecord::new("agent-a", "new work", json!({})),
///     CheckpointRecord::new("agent-b", "other work", json!({})),
/// ];
///
/// let latest = filter_latest_checkpoints(&cps);
/// assert_eq!(latest.len(), 2); // one per agent
/// ```
pub fn filter_latest_checkpoints(checkpoints: &[CheckpointRecord]) -> Vec<&CheckpointRecord> {
    let mut latest: HashMap<&str, &CheckpointRecord> = HashMap::new();

    for cp in checkpoints {
        let entry = latest.entry(cp.agent.as_str()).or_insert(cp);
        if cp.created_at > entry.created_at {
            *entry = cp;
        }
    }

    let mut result: Vec<&CheckpointRecord> = latest.into_values().collect();
    result.sort_by_key(|cp| &cp.agent);
    result
}

/// Builds the memory file name for a checkpoint.
///
/// Format: `checkpoint_{agent}_{YYYY-MM-DD}` where agent has slashes
/// replaced with underscores for filename safety.
fn checkpoint_memory_name(checkpoint: &CheckpointRecord) -> String {
    let date = format_date_from_unix(checkpoint.created_at);
    let safe_agent = checkpoint.agent.replace('/', "_");
    format!("checkpoint_{safe_agent}_{date}")
}

/// Formats a Unix timestamp as `YYYY-MM-DD`.
///
/// Falls back to `"unknown"` if the timestamp is invalid (negative
/// or out of range).
fn format_date_from_unix(timestamp: i64) -> String {
    // Manual UTC date calculation to avoid pulling in chrono
    if timestamp < 0 {
        return "unknown".to_string();
    }

    let days_since_epoch = timestamp / 86400;
    date_from_days(days_since_epoch)
}

/// Converts days since Unix epoch (1970-01-01) to `YYYY-MM-DD`.
fn date_from_days(mut days: i64) -> String {
    // Civil calendar algorithm from Howard Hinnant
    // https://howardhinnant.github.io/date_algorithms.html
    days += 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = days - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}")
}

/// Formats the full content body for a checkpoint memory file.
///
/// The body includes:
/// 1. A header with `working_on`
/// 2. Extracted `decisions` from state JSON (if present)
/// 3. Extracted `flags` from state JSON (if present)
/// 4. Extracted `next_steps` from state JSON (if present)
fn format_checkpoint_content(checkpoint: &CheckpointRecord) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Working on
    parts.push(format!("## Working On\n\n{}", checkpoint.working_on));

    // Extract structured fields from state JSON
    let decisions = extract_string_list(&checkpoint.state, "decisions");
    let flags = extract_string_list(&checkpoint.state, "flags");
    let next_steps = extract_string_list(&checkpoint.state, "next_steps");

    if !decisions.is_empty() {
        parts.push(format!(
            "## Decisions\n\n{}",
            format_as_bullet_list(&decisions)
        ));
    }

    if !flags.is_empty() {
        parts.push(format!("## Flags\n\n{}", format_as_bullet_list(&flags)));
    }

    if !next_steps.is_empty() {
        parts.push(format!(
            "## Next Steps\n\n{}",
            format_as_bullet_list(&next_steps)
        ));
    }

    // If the state has other interesting top-level keys, include
    // them as a JSON block for reference
    let extra_keys = extract_extra_keys(&checkpoint.state, &["decisions", "flags", "next_steps"]);
    if !extra_keys.is_null() {
        let json_str =
            serde_json::to_string_pretty(&extra_keys).unwrap_or_else(|_| "{}".to_string());
        parts.push(format!("## Additional State\n\n```json\n{json_str}\n```"));
    }

    parts.join("\n\n")
}

/// Extracts a list of strings from a JSON value at the given key.
///
/// Handles two common shapes:
/// - Array of strings: `["item1", "item2"]`
/// - Object with string values: `{"key1": "val1", "key2": "val2"}`
///   (returns `"key1: val1"` formatted entries)
///
/// Returns an empty vec for missing keys, null values, or
/// unrecognized shapes.
fn extract_string_list(state: &serde_json::Value, key: &str) -> Vec<String> {
    let Some(val) = state.get(key) else {
        return Vec::new();
    };

    match val {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        serde_json::Value::Object(obj) => obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| format!("{k}: {s}")))
            .collect(),
        _ => Vec::new(),
    }
}

/// Formats a list of strings as a Markdown bullet list.
fn format_as_bullet_list(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extracts top-level keys from a JSON object that are NOT in the
/// `exclude` list. Returns a new JSON object with only those keys.
fn extract_extra_keys(state: &serde_json::Value, exclude: &[&str]) -> serde_json::Value {
    let Some(obj) = state.as_object() else {
        return serde_json::Value::Null;
    };

    let filtered: serde_json::Map<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| !exclude.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if filtered.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(filtered)
    }
}

/// Formats a concise hook string for a checkpoint's MEMORY.md index
/// entry.
///
/// The hook is the `working_on` text, truncated to fit within the
/// 150-character line limit (leaving room for the title and link
/// markup).
fn format_checkpoint_hook(checkpoint: &CheckpointRecord) -> String {
    truncate_to_words(&checkpoint.working_on, 80)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{CheckpointRecord, LessonRecord};

    // --- severity_to_memory_type tests ---

    #[test]
    fn test_critical_maps_to_feedback() {
        assert_eq!(severity_to_memory_type("critical"), MemoryType::Feedback);
    }

    #[test]
    fn test_warning_maps_to_feedback() {
        assert_eq!(severity_to_memory_type("warning"), MemoryType::Feedback);
    }

    #[test]
    fn test_info_maps_to_project() {
        assert_eq!(severity_to_memory_type("info"), MemoryType::Project);
    }

    #[test]
    fn test_severity_case_insensitive() {
        assert_eq!(severity_to_memory_type("CRITICAL"), MemoryType::Feedback);
        assert_eq!(severity_to_memory_type("Warning"), MemoryType::Feedback);
        assert_eq!(severity_to_memory_type("INFO"), MemoryType::Project);
    }

    #[test]
    fn test_unknown_severity_maps_to_project() {
        assert_eq!(severity_to_memory_type("debug"), MemoryType::Project);
        assert_eq!(severity_to_memory_type(""), MemoryType::Project);
        assert_eq!(severity_to_memory_type("error"), MemoryType::Project);
    }

    // --- truncate_description tests ---

    #[test]
    fn test_truncate_short_content() {
        let result = truncate_description("Short content here.");
        assert_eq!(result, "Short content here.");
    }

    #[test]
    fn test_truncate_exactly_at_limit() {
        let content = "x".repeat(MAX_DESCRIPTION_LENGTH);
        let result = truncate_description(&content);
        assert_eq!(result.len(), MAX_DESCRIPTION_LENGTH);
    }

    #[test]
    fn test_truncate_long_content_at_word_boundary() {
        let content = "This is a moderately long piece of content that \
                        will definitely exceed the one hundred character \
                        limit we have set for descriptions in memory files";
        let result = truncate_description(content);
        assert!(result.len() <= MAX_DESCRIPTION_LENGTH);
        assert!(result.ends_with("..."));
        // Should not end mid-word
        assert!(!result.ends_with("a..."));
    }

    #[test]
    fn test_truncate_multiline_takes_first_line() {
        let content = "First line is short.\nSecond line has more detail.";
        let result = truncate_description(content);
        assert_eq!(result, "First line is short.");
    }

    #[test]
    fn test_truncate_strips_markdown_prefix() {
        assert_eq!(truncate_description("# Heading"), "Heading");
        assert_eq!(truncate_description("**Bold text**"), "Bold text**");
        assert_eq!(truncate_description("- List item"), "List item");
    }

    #[test]
    fn test_truncate_empty_content() {
        assert_eq!(truncate_description(""), "");
    }

    #[test]
    fn test_truncate_no_spaces_long_word() {
        let content = "a".repeat(200);
        let result = truncate_description(&content);
        assert!(result.len() <= MAX_DESCRIPTION_LENGTH);
        assert!(result.ends_with("..."));
    }

    // --- lesson_to_memory tests ---

    #[test]
    fn test_lesson_to_memory_critical() {
        let lesson = LessonRecord::new(
            "Never use unwrap",
            "Using unwrap() in production causes panics.",
            vec!["rust".into(), "error-handling".into()],
        )
        .with_severity("critical");

        let mf = lesson_to_memory(&lesson);

        assert_eq!(mf.name, "Never use unwrap");
        assert_eq!(mf.memory_type, MemoryType::Feedback);
        assert!(mf.content.contains("**[CRITICAL]**"));
        assert!(mf.content.contains("`rust`"));
        assert!(mf.content.contains("`error-handling`"));
        assert!(mf
            .content
            .contains("Using unwrap() in production causes panics."));
    }

    #[test]
    fn test_lesson_to_memory_warning() {
        let lesson = LessonRecord::new(
            "Avoid println in lib code",
            "Use tracing instead of println in library code.",
            vec!["rust".into()],
        )
        .with_severity("warning");

        let mf = lesson_to_memory(&lesson);

        assert_eq!(mf.memory_type, MemoryType::Feedback);
        assert!(mf.content.contains("**[WARNING]**"));
    }

    #[test]
    fn test_lesson_to_memory_info() {
        let lesson = LessonRecord::new(
            "SQLite WAL mode",
            "WAL mode improves concurrent read performance.",
            vec!["sqlite".into()],
        )
        .with_severity("info");

        let mf = lesson_to_memory(&lesson);

        assert_eq!(mf.memory_type, MemoryType::Project);
        assert!(mf.content.contains("**[INFO]**"));
    }

    #[test]
    fn test_lesson_to_memory_no_tags() {
        let lesson = LessonRecord::new("Simple lesson", "Content without tags.", vec![]);

        let mf = lesson_to_memory(&lesson);

        assert!(!mf.content.contains("Tags:"));
        assert!(mf.content.contains("Content without tags."));
    }

    #[test]
    fn test_lesson_to_memory_description_truncated() {
        let long_content = "x ".repeat(200); // 400 chars
        let lesson = LessonRecord::new("Long Content", &long_content, vec![]);

        let mf = lesson_to_memory(&lesson);

        assert!(
            mf.description.len() <= MAX_DESCRIPTION_LENGTH,
            "description should be at most {} chars, got {}",
            MAX_DESCRIPTION_LENGTH,
            mf.description.len()
        );
    }

    #[test]
    fn test_lesson_to_memory_filename_matches() {
        let lesson = LessonRecord::new("Git Interleaving Rules", "content", vec![]);

        let mf = lesson_to_memory(&lesson);

        assert_eq!(mf.filename(), "git_interleaving_rules.md");
    }

    // --- lesson_to_index_entry tests ---

    #[test]
    fn test_index_entry_title() {
        let lesson = LessonRecord::new("My Lesson", "Some content", vec![]);

        let (title, _filename, _hook) = lesson_to_index_entry(&lesson);

        assert_eq!(title, "My Lesson");
    }

    #[test]
    fn test_index_entry_filename() {
        let lesson = LessonRecord::new("SQLite: WAL Mode (v2)", "content", vec![]);

        let (_title, filename, _hook) = lesson_to_index_entry(&lesson);

        assert_eq!(filename, "sqlite_wal_mode_v2.md");
    }

    #[test]
    fn test_index_entry_hook_critical() {
        let lesson = LessonRecord::new(
            "Critical Bug",
            "This is a critical production issue.",
            vec![],
        )
        .with_severity("critical");

        let (_title, _filename, hook) = lesson_to_index_entry(&lesson);

        assert!(
            hook.starts_with("[critical]"),
            "hook should start with [critical], got: {hook}"
        );
    }

    #[test]
    fn test_index_entry_hook_info_no_prefix() {
        let lesson = LessonRecord::new("General Info", "Just some project knowledge.", vec![])
            .with_severity("info");

        let (_title, _filename, hook) = lesson_to_index_entry(&lesson);

        assert!(
            !hook.starts_with('['),
            "info hook should not have severity prefix, got: {hook}"
        );
    }

    #[test]
    fn test_index_entry_hook_length() {
        let long_content = "word ".repeat(100);
        let lesson = LessonRecord::new("Verbose Lesson", &long_content, vec![]);

        let (_title, _filename, hook) = lesson_to_index_entry(&lesson);

        assert!(
            hook.len() <= 80,
            "hook should be at most 80 chars, got {}",
            hook.len()
        );
    }

    // --- lesson_memory_filename tests ---

    #[test]
    fn test_lesson_memory_filename_basic() {
        assert_eq!(
            lesson_memory_filename("My Cool Lesson"),
            "my_cool_lesson.md"
        );
    }

    #[test]
    fn test_lesson_memory_filename_special_chars() {
        assert_eq!(
            lesson_memory_filename("SQLite: WAL Mode (v2)"),
            "sqlite_wal_mode_v2.md"
        );
    }

    #[test]
    fn test_lesson_memory_filename_empty() {
        assert_eq!(lesson_memory_filename(""), "_unnamed.md");
    }

    #[test]
    fn test_lesson_memory_filename_hyphens() {
        assert_eq!(
            lesson_memory_filename("pre-commit hooks"),
            "pre-commit_hooks.md"
        );
    }

    // --- format_lesson_content tests ---

    #[test]
    fn test_content_has_severity_tag() {
        let lesson = LessonRecord::new("Test", "Body text.", vec![]).with_severity("warning");
        let content = format_lesson_content(&lesson);
        assert!(content.starts_with("**[WARNING]**"));
    }

    #[test]
    fn test_content_has_tags() {
        let lesson = LessonRecord::new("Test", "Body text.", vec!["rust".into(), "sqlite".into()]);
        let content = format_lesson_content(&lesson);
        assert!(content.contains("Tags: `rust`, `sqlite`"));
    }

    #[test]
    fn test_content_no_tags_when_empty() {
        let lesson = LessonRecord::new("Test", "Body text.", vec![]);
        let content = format_lesson_content(&lesson);
        assert!(!content.contains("Tags:"));
    }

    #[test]
    fn test_content_includes_full_body() {
        let body = "This is the full lesson content.\n\nWith multiple paragraphs.";
        let lesson = LessonRecord::new("Test", body, vec![]);
        let content = format_lesson_content(&lesson);
        assert!(content.contains(body));
    }

    // --- format_lesson_hook tests ---

    #[test]
    fn test_hook_critical_prefix() {
        let lesson =
            LessonRecord::new("T", "Something bad happened.", vec![]).with_severity("critical");
        let hook = format_lesson_hook(&lesson);
        assert!(hook.starts_with("[critical] "));
    }

    #[test]
    fn test_hook_warning_prefix() {
        let lesson =
            LessonRecord::new("T", "Be careful with this.", vec![]).with_severity("warning");
        let hook = format_lesson_hook(&lesson);
        assert!(hook.starts_with("[warning] "));
    }

    #[test]
    fn test_hook_info_no_prefix() {
        let lesson = LessonRecord::new("T", "Just some info.", vec![]).with_severity("info");
        let hook = format_lesson_hook(&lesson);
        assert_eq!(hook, "Just some info.");
    }

    // --- truncate_to_words tests ---

    #[test]
    fn test_truncate_to_words_short() {
        assert_eq!(truncate_to_words("short text", 80), "short text");
    }

    #[test]
    fn test_truncate_to_words_long() {
        let text = "a very long sentence that goes on and on and on and on \
                     and keeps going until it exceeds the limit we set";
        let result = truncate_to_words(text, 40);
        assert!(result.len() <= 40);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_to_words_multiline() {
        let text = "first line only\nsecond line ignored";
        let result = truncate_to_words(text, 80);
        assert_eq!(result, "first line only");
    }

    // --- Edge cases / integration ---

    #[test]
    fn test_roundtrip_lesson_to_memory_and_index() {
        let lesson = LessonRecord::new(
            "Git Interleaving Rules",
            "Always create one branch per task, one commit per subtask.",
            vec!["git".into(), "workflow".into()],
        )
        .with_severity("warning");

        let mf = lesson_to_memory(&lesson);
        let (title, filename, hook) = lesson_to_index_entry(&lesson);

        // Memory file should be consistent
        assert_eq!(mf.name, title);
        assert_eq!(mf.filename(), filename);

        // Hook should reference the severity
        assert!(hook.contains("[warning]"));

        // Content should have everything
        assert!(mf.content.contains("**[WARNING]**"));
        assert!(mf.content.contains("`git`"));
        assert!(mf.content.contains("Always create one branch"));
    }

    #[test]
    fn test_distinct_titles_produce_distinct_filenames() {
        let titles = [
            "SQLite WAL mode",
            "SQLite journal mode",
            "Git branching strategy",
            "Rust error handling",
            "Pre-commit hooks",
        ];

        let filenames: Vec<String> = titles.iter().map(|t| lesson_memory_filename(t)).collect();

        // All filenames should be unique
        let mut unique = filenames.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            filenames.len(),
            unique.len(),
            "expected all filenames to be unique, got duplicates"
        );
    }

    #[test]
    fn test_filenames_are_filesystem_safe() {
        let tricky_titles = [
            "Path: /home/user/file.rs",
            "Question? Answer!",
            "50% complete",
            "foo/bar\\baz",
            "tabs\there",
            "null\0byte",
        ];

        for title in &tricky_titles {
            let filename = lesson_memory_filename(title);
            // Must not contain filesystem-unsafe characters
            assert!(
                !filename.contains('/'),
                "filename contains /: {filename} (from {title})"
            );
            assert!(
                !filename.contains('\\'),
                "filename contains \\: {filename} (from {title})"
            );
            assert!(
                !filename.contains('\0'),
                "filename contains null: {filename} (from {title})"
            );
            assert!(
                filename.ends_with(".md"),
                "filename doesn't end with .md: {filename} (from {title})"
            );
        }
    }

    #[test]
    fn test_very_long_title() {
        let long_title = "a ".repeat(500); // 1000 chars
        let filename = lesson_memory_filename(&long_title);

        // Should still produce a valid filename (just very long)
        assert!(filename.ends_with(".md"));
        assert!(!filename.contains(' '));

        // The memory file should also work
        let lesson = LessonRecord::new(&long_title, "content", vec![]);
        let mf = lesson_to_memory(&lesson);
        assert!(!mf.filename().contains(' '));
    }

    // ================================================================
    // Checkpoint mapping tests
    // ================================================================

    /// Helper to build a checkpoint with a known timestamp for
    /// deterministic date output.
    fn make_checkpoint(
        agent: &str,
        working_on: &str,
        state: serde_json::Value,
        created_at: i64,
    ) -> CheckpointRecord {
        let mut cp = CheckpointRecord::new(agent, working_on, state);
        cp.created_at = created_at;
        cp
    }

    // --- checkpoint_memory_name tests ---

    #[test]
    fn test_checkpoint_name_format() {
        // 2025-06-16 = Unix 1750032000
        let cp = make_checkpoint(
            "user/my-project",
            "working on stuff",
            serde_json::json!({}),
            1_750_032_000,
        );
        let name = checkpoint_memory_name(&cp);
        assert_eq!(name, "checkpoint_user_my-project_2025-06-16");
    }

    #[test]
    fn test_checkpoint_name_simple_agent() {
        let cp = make_checkpoint("test-agent", "work", serde_json::json!({}), 1_750_032_000);
        let name = checkpoint_memory_name(&cp);
        assert_eq!(name, "checkpoint_test-agent_2025-06-16");
    }

    #[test]
    fn test_checkpoint_name_agent_with_slashes() {
        let cp = make_checkpoint(
            "org/team/project",
            "work",
            serde_json::json!({}),
            1_750_032_000,
        );
        let name = checkpoint_memory_name(&cp);
        // Slashes replaced with underscores
        assert_eq!(name, "checkpoint_org_team_project_2025-06-16");
    }

    // --- format_date_from_unix tests ---

    #[test]
    fn test_date_epoch() {
        assert_eq!(format_date_from_unix(0), "1970-01-01");
    }

    #[test]
    fn test_date_known_value() {
        // 2025-06-16 00:00:00 UTC = 1750032000
        assert_eq!(format_date_from_unix(1_750_032_000), "2025-06-16");
    }

    #[test]
    fn test_date_another_known_value() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        assert_eq!(format_date_from_unix(1_704_067_200), "2024-01-01");
    }

    #[test]
    fn test_date_negative_timestamp() {
        assert_eq!(format_date_from_unix(-1), "unknown");
    }

    #[test]
    fn test_date_recent() {
        // 2026-03-31 = roughly 1774828800
        // Just verify it produces a valid-looking date
        let result = format_date_from_unix(1_774_828_800);
        assert!(
            result.len() == 10,
            "date should be YYYY-MM-DD, got: {result}"
        );
        assert!(result.starts_with("2026-"), "expected 2026, got: {result}");
    }

    // --- checkpoint_to_memory tests ---

    #[test]
    fn test_checkpoint_to_memory_basic() {
        let cp = make_checkpoint(
            "user/my-project",
            "Implementing deep hooks",
            serde_json::json!({
                "decisions": ["Use atomic writes", "Tag with [nellie]"],
                "flags": ["needs_review"],
                "next_steps": ["Implement sync command"]
            }),
            1_750_032_000,
        );

        let mf = checkpoint_to_memory(&cp);

        assert_eq!(mf.name, "checkpoint_user_my-project_2025-06-16");
        assert_eq!(mf.memory_type, MemoryType::Project);
        assert_eq!(mf.description, "Implementing deep hooks");
        assert!(mf.content.contains("## Working On"));
        assert!(mf.content.contains("Implementing deep hooks"));
        assert!(mf.content.contains("## Decisions"));
        assert!(mf.content.contains("- Use atomic writes"));
        assert!(mf.content.contains("- Tag with [nellie]"));
        assert!(mf.content.contains("## Flags"));
        assert!(mf.content.contains("- needs_review"));
        assert!(mf.content.contains("## Next Steps"));
        assert!(mf.content.contains("- Implement sync command"));
    }

    #[test]
    fn test_checkpoint_to_memory_empty_state() {
        let cp = make_checkpoint(
            "agent",
            "Just working",
            serde_json::json!({}),
            1_750_032_000,
        );

        let mf = checkpoint_to_memory(&cp);

        assert!(mf.content.contains("## Working On"));
        assert!(mf.content.contains("Just working"));
        // No decisions/flags/next_steps sections
        assert!(!mf.content.contains("## Decisions"));
        assert!(!mf.content.contains("## Flags"));
        assert!(!mf.content.contains("## Next Steps"));
    }

    #[test]
    fn test_checkpoint_to_memory_null_state() {
        let cp = make_checkpoint("agent", "Working", serde_json::Value::Null, 1_750_032_000);

        let mf = checkpoint_to_memory(&cp);

        assert!(mf.content.contains("## Working On"));
        assert!(!mf.content.contains("## Decisions"));
    }

    #[test]
    fn test_checkpoint_to_memory_partial_state() {
        let cp = make_checkpoint(
            "agent",
            "Partial state test",
            serde_json::json!({
                "decisions": ["Decision A"],
                // no flags or next_steps
            }),
            1_750_032_000,
        );

        let mf = checkpoint_to_memory(&cp);

        assert!(mf.content.contains("## Decisions"));
        assert!(mf.content.contains("- Decision A"));
        assert!(!mf.content.contains("## Flags"));
        assert!(!mf.content.contains("## Next Steps"));
    }

    #[test]
    fn test_checkpoint_to_memory_object_state_fields() {
        // State fields can be objects with string values
        let cp = make_checkpoint(
            "agent",
            "Object state test",
            serde_json::json!({
                "decisions": {
                    "db": "Use SQLite",
                    "format": "Use YAML frontmatter"
                }
            }),
            1_750_032_000,
        );

        let mf = checkpoint_to_memory(&cp);

        assert!(mf.content.contains("## Decisions"));
        assert!(mf.content.contains("- db: Use SQLite"));
        assert!(mf.content.contains("- format: Use YAML frontmatter"));
    }

    #[test]
    fn test_checkpoint_to_memory_extra_state_keys() {
        let cp = make_checkpoint(
            "agent",
            "Extra keys test",
            serde_json::json!({
                "decisions": ["A"],
                "custom_field": "custom_value",
                "another": 42
            }),
            1_750_032_000,
        );

        let mf = checkpoint_to_memory(&cp);

        assert!(mf.content.contains("## Decisions"));
        assert!(mf.content.contains("## Additional State"));
        assert!(mf.content.contains("custom_field"));
        assert!(mf.content.contains("custom_value"));
    }

    #[test]
    fn test_checkpoint_to_memory_no_extra_keys() {
        let cp = make_checkpoint(
            "agent",
            "No extras",
            serde_json::json!({
                "decisions": ["A"],
                "flags": ["B"],
                "next_steps": ["C"]
            }),
            1_750_032_000,
        );

        let mf = checkpoint_to_memory(&cp);

        // Only known keys, so no "Additional State" section
        assert!(!mf.content.contains("## Additional State"));
    }

    #[test]
    fn test_checkpoint_to_memory_long_working_on() {
        let long_desc = "x ".repeat(200);
        let cp = make_checkpoint("agent", &long_desc, serde_json::json!({}), 1_750_032_000);

        let mf = checkpoint_to_memory(&cp);

        assert!(
            mf.description.len() <= MAX_DESCRIPTION_LENGTH,
            "description should be at most {} chars, got {}",
            MAX_DESCRIPTION_LENGTH,
            mf.description.len()
        );
    }

    #[test]
    fn test_checkpoint_to_memory_type_always_project() {
        let cp = make_checkpoint("agent", "work", serde_json::json!({}), 1_750_032_000);
        let mf = checkpoint_to_memory(&cp);
        assert_eq!(mf.memory_type, MemoryType::Project);
    }

    // --- checkpoint_to_index_entry tests ---

    #[test]
    fn test_checkpoint_index_entry_title() {
        let cp = make_checkpoint(
            "user/my-project",
            "Deep hooks phase 1",
            serde_json::json!({}),
            1_750_032_000,
        );

        let (title, _filename, _hook) = checkpoint_to_index_entry(&cp);

        assert_eq!(title, "checkpoint_user_my-project_2025-06-16");
    }

    #[test]
    fn test_checkpoint_index_entry_filename() {
        let cp = make_checkpoint(
            "user/my-project",
            "work",
            serde_json::json!({}),
            1_750_032_000,
        );

        let (_title, filename, _hook) = checkpoint_to_index_entry(&cp);

        // Filename should be a valid .md file
        assert!(filename.ends_with(".md"));
        assert!(!filename.contains('/'));
        assert!(!filename.contains(' '));
    }

    #[test]
    fn test_checkpoint_index_entry_hook() {
        let cp = make_checkpoint(
            "agent",
            "Implementing the sync command for deep hooks",
            serde_json::json!({}),
            1_750_032_000,
        );

        let (_title, _filename, hook) = checkpoint_to_index_entry(&cp);

        assert_eq!(hook, "Implementing the sync command for deep hooks");
        assert!(hook.len() <= 80);
    }

    #[test]
    fn test_checkpoint_index_entry_hook_truncated() {
        let long_work = "a ".repeat(100); // 200 chars
        let cp = make_checkpoint("agent", &long_work, serde_json::json!({}), 1_750_032_000);

        let (_title, _filename, hook) = checkpoint_to_index_entry(&cp);

        assert!(
            hook.len() <= 80,
            "hook should be at most 80 chars, got {}",
            hook.len()
        );
    }

    // --- filter_latest_checkpoints tests ---

    #[test]
    fn test_filter_single_agent_single_checkpoint() {
        let cp = make_checkpoint("agent-a", "work", serde_json::json!({}), 1000);
        let cps = [cp];

        let result = filter_latest_checkpoints(&cps);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].agent, "agent-a");
    }

    #[test]
    fn test_filter_single_agent_multiple_checkpoints() {
        let cp_old = make_checkpoint("agent-a", "old work", serde_json::json!({}), 1000);
        let cp_new = make_checkpoint("agent-a", "new work", serde_json::json!({}), 2000);

        let cps = vec![cp_old, cp_new];
        let result = filter_latest_checkpoints(&cps);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].working_on, "new work");
        assert_eq!(result[0].created_at, 2000);
    }

    #[test]
    fn test_filter_multiple_agents() {
        let cp_a1 = make_checkpoint("agent-a", "a old", serde_json::json!({}), 1000);
        let cp_a2 = make_checkpoint("agent-a", "a new", serde_json::json!({}), 3000);
        let cp_b = make_checkpoint("agent-b", "b only", serde_json::json!({}), 2000);

        let cps = vec![cp_a1, cp_a2, cp_b];
        let result = filter_latest_checkpoints(&cps);

        assert_eq!(result.len(), 2);
        // Sorted by agent name
        assert_eq!(result[0].agent, "agent-a");
        assert_eq!(result[0].working_on, "a new");
        assert_eq!(result[1].agent, "agent-b");
        assert_eq!(result[1].working_on, "b only");
    }

    #[test]
    fn test_filter_empty_list() {
        let result = filter_latest_checkpoints(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_preserves_latest_even_if_first() {
        // Latest checkpoint appears first in input
        let cp_new = make_checkpoint("agent", "newest", serde_json::json!({}), 5000);
        let cp_mid = make_checkpoint("agent", "middle", serde_json::json!({}), 3000);
        let cp_old = make_checkpoint("agent", "oldest", serde_json::json!({}), 1000);

        let cps = vec![cp_new, cp_mid, cp_old];
        let result = filter_latest_checkpoints(&cps);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].working_on, "newest");
    }

    #[test]
    fn test_filter_many_agents() {
        let cps: Vec<CheckpointRecord> = (0..10)
            .map(|i| {
                make_checkpoint(
                    &format!("agent-{i:02}"),
                    &format!("work {i}"),
                    serde_json::json!({}),
                    i64::from(i) * 1000,
                )
            })
            .collect();

        let result = filter_latest_checkpoints(&cps);
        assert_eq!(result.len(), 10);
        // Sorted by agent name
        assert_eq!(result[0].agent, "agent-00");
        assert_eq!(result[9].agent, "agent-09");
    }

    // --- extract_string_list tests ---

    #[test]
    fn test_extract_array_of_strings() {
        let state = serde_json::json!({
            "decisions": ["A", "B", "C"]
        });
        let result = extract_string_list(&state, "decisions");
        assert_eq!(result, vec!["A", "B", "C"]);
    }

    #[test]
    fn test_extract_object_values() {
        let state = serde_json::json!({
            "flags": {"active": "true", "debug": "false"}
        });
        let result = extract_string_list(&state, "flags");
        // Object keys are not guaranteed order, so just check
        // contents
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|s| s == "active: true"));
        assert!(result.iter().any(|s| s == "debug: false"));
    }

    #[test]
    fn test_extract_missing_key() {
        let state = serde_json::json!({"other": "value"});
        let result = extract_string_list(&state, "decisions");
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_null_value() {
        let state = serde_json::json!({"decisions": null});
        let result = extract_string_list(&state, "decisions");
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_number_value() {
        let state = serde_json::json!({"decisions": 42});
        let result = extract_string_list(&state, "decisions");
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_from_null_state() {
        let state = serde_json::Value::Null;
        let result = extract_string_list(&state, "decisions");
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_mixed_array() {
        // Array with non-string elements should skip them
        let state = serde_json::json!({
            "items": ["valid", 42, "also valid", null, true]
        });
        let result = extract_string_list(&state, "items");
        assert_eq!(result, vec!["valid", "also valid"]);
    }

    // --- extract_extra_keys tests ---

    #[test]
    fn test_extra_keys_excludes_known() {
        let state = serde_json::json!({
            "decisions": ["A"],
            "flags": ["B"],
            "next_steps": ["C"],
            "custom": "value"
        });

        let extra = extract_extra_keys(&state, &["decisions", "flags", "next_steps"]);

        assert!(extra.is_object());
        let obj = extra.as_object().expect("should be object");
        assert_eq!(obj.len(), 1);
        assert_eq!(obj["custom"], "value");
    }

    #[test]
    fn test_extra_keys_all_excluded() {
        let state = serde_json::json!({
            "decisions": ["A"],
            "flags": ["B"]
        });

        let extra = extract_extra_keys(&state, &["decisions", "flags", "next_steps"]);

        assert!(extra.is_null());
    }

    #[test]
    fn test_extra_keys_from_null() {
        let extra = extract_extra_keys(&serde_json::Value::Null, &["decisions"]);
        assert!(extra.is_null());
    }

    // --- format_as_bullet_list tests ---

    #[test]
    fn test_bullet_list_single_item() {
        let items = vec!["Item one".to_string()];
        assert_eq!(format_as_bullet_list(&items), "- Item one");
    }

    #[test]
    fn test_bullet_list_multiple_items() {
        let items = vec![
            "First".to_string(),
            "Second".to_string(),
            "Third".to_string(),
        ];
        assert_eq!(format_as_bullet_list(&items), "- First\n- Second\n- Third");
    }

    #[test]
    fn test_bullet_list_empty() {
        let items: Vec<String> = vec![];
        assert_eq!(format_as_bullet_list(&items), "");
    }

    // --- Integration / roundtrip tests ---

    #[test]
    fn test_checkpoint_roundtrip_memory_and_index() {
        let cp = make_checkpoint(
            "user/my-project",
            "Implementing checkpoint mapper",
            serde_json::json!({
                "decisions": ["Parse state JSON", "Use Project type"],
                "next_steps": ["Write tests"]
            }),
            1_750_032_000,
        );

        let mf = checkpoint_to_memory(&cp);
        let (title, filename, hook) = checkpoint_to_index_entry(&cp);

        // Name consistency
        assert_eq!(mf.name, title);
        assert_eq!(mf.filename(), filename);

        // Hook contains working_on text
        assert!(hook.contains("checkpoint mapper"));

        // Content has all sections
        assert!(mf.content.contains("## Working On"));
        assert!(mf.content.contains("## Decisions"));
        assert!(mf.content.contains("## Next Steps"));
        assert!(!mf.content.contains("## Flags"));
    }

    #[test]
    fn test_checkpoint_filename_is_filesystem_safe() {
        let tricky_agents = [
            "user/my-project",
            "org/team/deep-project",
            "simple",
            "with spaces",
            "special!@#$%",
        ];

        for agent in &tricky_agents {
            let cp = make_checkpoint(agent, "work", serde_json::json!({}), 1_750_032_000);
            let mf = checkpoint_to_memory(&cp);
            let filename = mf.filename();

            assert!(
                !filename.contains('/'),
                "filename has /: {filename} (agent: {agent})"
            );
            assert!(
                !filename.contains('\\'),
                "filename has \\: {filename} (agent: {agent})"
            );
            assert!(
                filename.ends_with(".md"),
                "filename missing .md: {filename} (agent: {agent})"
            );
        }
    }

    #[test]
    fn test_distinct_agents_produce_distinct_filenames() {
        let agents = [
            "user/my-project",
            "user/other-project",
            "other-user/my-project",
        ];

        let filenames: Vec<String> = agents
            .iter()
            .map(|a| {
                let cp = make_checkpoint(a, "work", serde_json::json!({}), 1_750_032_000);
                checkpoint_to_memory(&cp).filename()
            })
            .collect();

        let mut unique = filenames.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            filenames.len(),
            unique.len(),
            "expected distinct filenames, got: {filenames:?}"
        );
    }

    #[test]
    fn test_same_agent_different_dates_produce_distinct_filenames() {
        let dates = [1_750_032_000i64, 1_750_118_400, 1_750_204_800];

        let filenames: Vec<String> = dates
            .iter()
            .map(|&ts| {
                let cp = make_checkpoint("user/my-project", "work", serde_json::json!({}), ts);
                checkpoint_to_memory(&cp).filename()
            })
            .collect();

        let mut unique = filenames.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            filenames.len(),
            unique.len(),
            "expected distinct filenames for different dates, got: {filenames:?}"
        );
    }
}
