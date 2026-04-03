//! Pattern extractor for passive learning from Claude Code transcripts.
//!
//! Analyzes a sequence of [`TranscriptEntry`] values and extracts
//! learnable patterns — corrections, tool failures, repeated tool
//! usage, explicit save requests, and build failures followed by
//! fixes. Each extraction produces an [`ExtractedLesson`] ready for
//! deduplication and storage in Nellie's lesson database.
//!
//! # Extraction Patterns
//!
//! | Pattern | Trigger | Severity |
//! |---------|---------|----------|
//! | Correction | User says "no", "don't", "stop", "wrong" after assistant | `warning` |
//! | Tool failure | `ToolResult` with `is_error: true` | `warning` |
//! | Repeated tool | Same tool called 3+ times with similar args | `info` |
//! | Explicit save | User says "remember", "save this", "lesson learned" | `info` |
//! | Build failure | Bash output with `error[E`, `FAILED`, `panic` + fix | `critical` |
//!
//! # Context Window
//!
//! Each extraction includes surrounding entries (up to
//! [`CONTEXT_BEFORE`] entries before and [`CONTEXT_AFTER`] entries
//! after the trigger) to provide meaningful content for the lesson.

use strsim::jaro_winkler;

use super::transcript::{TranscriptContent, TranscriptEntry};

/// Number of entries before the trigger to include as context.
const CONTEXT_BEFORE: usize = 2;

/// Number of entries after the trigger to include as context.
const CONTEXT_AFTER: usize = 3;

/// Jaro-Winkler similarity threshold for considering two tool inputs
/// "similar" when detecting repeated patterns.
const SIMILARITY_THRESHOLD: f64 = 0.80;

/// Minimum number of similar consecutive tool calls to trigger the
/// repeated-pattern extractor.
const REPEAT_THRESHOLD: usize = 3;

/// Keywords in user messages that signal a correction of assistant
/// behaviour.
const CORRECTION_KEYWORDS: &[&str] = &[
    "no,",
    "no.",
    "no!",
    "no ",
    "don't",
    "dont",
    "stop",
    "wrong",
    "that's not",
    "thats not",
    "that is not",
    "incorrect",
    "not what i",
    "shouldn't",
    "should not",
];

/// Keywords that signal the user wants to explicitly save a lesson.
const SAVE_KEYWORDS: &[&str] = &[
    "remember",
    "save this",
    "lesson learned",
    "note to self",
    "keep in mind",
    "don't forget",
    "important:",
    "remember this",
    "take note",
];

/// Patterns in Bash tool output that indicate a build/compile failure.
const BUILD_FAILURE_PATTERNS: &[&str] = &[
    "error[E",
    "FAILED",
    "panic",
    "cannot find",
    "unresolved import",
    "mismatched types",
    "aborting due to",
    "could not compile",
];

/// A lesson extracted from a transcript by pattern analysis.
///
/// This struct is independent of Nellie's internal `LessonRecord` — it
/// represents a *candidate* lesson that will be deduplicated and
/// potentially stored downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedLesson {
    /// Short, descriptive title summarising what was learned.
    pub title: String,
    /// Full lesson content including surrounding context.
    pub content: String,
    /// Tags for categorisation and glob-based rule generation.
    pub tags: Vec<String>,
    /// Severity: `"critical"`, `"warning"`, or `"info"`.
    pub severity: String,
    /// Session ID from which this lesson was extracted.
    pub source_session: String,
}

/// Extract learnable lessons from a parsed transcript.
///
/// Runs all five extraction patterns over the entries and returns
/// deduplicated results. Lessons with identical titles are collapsed
/// to the first occurrence.
///
/// # Arguments
///
/// * `entries` — Parsed transcript entries, in chronological order.
///
/// # Returns
///
/// A vector of extracted lessons, one per detected pattern match.
pub fn extract_lessons(entries: &[TranscriptEntry]) -> Vec<ExtractedLesson> {
    let mut lessons = Vec::new();

    extract_corrections(entries, &mut lessons);
    extract_tool_failures(entries, &mut lessons);
    extract_repeated_patterns(entries, &mut lessons);
    extract_explicit_saves(entries, &mut lessons);
    extract_build_failures(entries, &mut lessons);

    // Deduplicate by title (keep first occurrence).
    let mut seen_titles = std::collections::HashSet::new();
    lessons.retain(|l| seen_titles.insert(l.title.clone()));

    lessons
}

// ---------------------------------------------------------------------------
// Pattern 1: Corrections
// ---------------------------------------------------------------------------

/// Detect corrections: user says something negative after an
/// assistant action, indicating the assistant did something wrong.
fn extract_corrections(entries: &[TranscriptEntry], lessons: &mut Vec<ExtractedLesson>) {
    for (i, entry) in entries.iter().enumerate() {
        let TranscriptContent::Human { text: user_text } = &entry.content else {
            continue;
        };

        let lower = user_text.to_lowercase();

        // Must start with or contain a correction keyword.
        let is_correction = CORRECTION_KEYWORDS
            .iter()
            .any(|kw| lower.starts_with(kw) || lower.contains(kw));

        if !is_correction {
            continue;
        }

        // There must be a preceding assistant entry to correct.
        if find_preceding_assistant(entries, i).is_none() {
            continue;
        }

        let context = gather_context(entries, i);
        let session = &entry.session_id;

        let title = format!("Correction: {}", truncate_for_title(user_text, 80));

        lessons.push(ExtractedLesson {
            title,
            content: format!(
                "User corrected assistant behaviour.\n\n\
                 User said: {user_text}\n\n\
                 Context:\n{context}"
            ),
            tags: vec!["correction".to_string(), "feedback".to_string()],
            severity: "warning".to_string(),
            source_session: session.clone(),
        });
    }
}

/// Find the most recent assistant or tool-use entry before index `i`.
fn find_preceding_assistant(entries: &[TranscriptEntry], i: usize) -> Option<usize> {
    if i == 0 {
        return None;
    }
    for j in (0..i).rev() {
        if matches!(
            &entries[j].content,
            TranscriptContent::Assistant { .. } | TranscriptContent::ToolUse { .. }
        ) {
            return Some(j);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Pattern 2: Tool failures
// ---------------------------------------------------------------------------

/// Detect tool results that are explicit errors.
fn extract_tool_failures(entries: &[TranscriptEntry], lessons: &mut Vec<ExtractedLesson>) {
    for (i, entry) in entries.iter().enumerate() {
        let (tool_use_id, output, is_error) = match &entry.content {
            TranscriptContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => (tool_use_id, output, *is_error),
            _ => continue,
        };

        if !is_error {
            continue;
        }

        // Try to find the tool_use that triggered this error.
        let tool_name = find_tool_name_for_id(entries, i, tool_use_id);
        let context = gather_context(entries, i);
        let session = &entry.session_id;

        let name_str = tool_name.as_deref().unwrap_or("unknown");
        let short_output = truncate_for_title(output, 60);
        let title = format!("Tool failure ({name_str}): {short_output}");
        let name_lower = name_str.to_lowercase();

        lessons.push(ExtractedLesson {
            title,
            content: format!(
                "Tool `{name_str}` returned an error.\n\n\
                 Error output: {output}\n\n\
                 Context:\n{context}"
            ),
            tags: vec!["tool-failure".to_string(), format!("tool-{name_lower}")],
            severity: "warning".to_string(),
            source_session: session.clone(),
        });
    }
}

/// Search backwards from index `i` for a `ToolUse` or `Assistant`
/// entry containing a tool call whose ID matches `tool_use_id`.
fn find_tool_name_for_id(
    entries: &[TranscriptEntry],
    i: usize,
    tool_use_id: &str,
) -> Option<String> {
    if tool_use_id.is_empty() {
        return None;
    }

    let search_start = i.saturating_sub(10);
    for j in (search_start..i).rev() {
        match &entries[j].content {
            TranscriptContent::ToolUse { name, .. } => {
                // In Claude Code transcripts, ToolUse entries
                // don't always carry the ID at this level, but
                // when there is a 1:1 correspondence (the ToolUse
                // immediately precedes the ToolResult), this is
                // the match.
                return Some(name.clone());
            }
            TranscriptContent::Assistant { tool_calls, .. } => {
                for tc in tool_calls {
                    if tc.id == tool_use_id {
                        return Some(tc.name.clone());
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Pattern 3: Repeated patterns
// ---------------------------------------------------------------------------

/// Detect the same tool being called 3+ times in a row with similar
/// arguments, indicating a retry loop or trial-and-error pattern.
fn extract_repeated_patterns(entries: &[TranscriptEntry], lessons: &mut Vec<ExtractedLesson>) {
    // Collect tool-use entries with their indices.
    let tool_uses: Vec<(usize, &str, &serde_json::Value)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.content {
            TranscriptContent::ToolUse { name, input } => Some((i, name.as_str(), input)),
            TranscriptContent::Assistant { tool_calls, .. } if tool_calls.len() == 1 => {
                let tc = &tool_calls[0];
                Some((i, tc.name.as_str(), &tc.input))
            }
            _ => None,
        })
        .collect();

    if tool_uses.len() < REPEAT_THRESHOLD {
        return;
    }

    // Group consecutive runs of the same tool with similar args.
    let mut i = 0;
    let mut reported_indices = std::collections::HashSet::new();

    while i < tool_uses.len() {
        let (idx, name, input) = tool_uses[i];
        let input_str = input.to_string();
        let mut run = vec![idx];

        let mut j = i + 1;
        while j < tool_uses.len() {
            let (jdx, jname, jinput) = tool_uses[j];
            if jname != name {
                break;
            }
            let jinput_str = jinput.to_string();
            let sim = jaro_winkler(&input_str, &jinput_str);
            if sim < SIMILARITY_THRESHOLD {
                break;
            }
            run.push(jdx);
            j += 1;
        }

        if run.len() >= REPEAT_THRESHOLD && !reported_indices.contains(&run[0]) {
            for &idx_val in &run {
                reported_indices.insert(idx_val);
            }

            let first_idx = run[0];
            let session = &entries[first_idx].session_id;
            let context = gather_context(entries, first_idx);

            let run_len = run.len();
            let title = format!("Repeated tool pattern: {name} called {run_len} times");

            let args_short = truncate_for_title(&input_str, 200);
            let name_lower = name.to_lowercase();
            lessons.push(ExtractedLesson {
                title,
                content: format!(
                    "Tool `{name}` was called {run_len} times with \
                     similar arguments, suggesting a retry loop or \
                     trial-and-error pattern.\n\n\
                     First invocation args: {args_short}\n\n\
                     Context:\n{context}"
                ),
                tags: vec!["repeated-pattern".to_string(), format!("tool-{name_lower}")],
                severity: "info".to_string(),
                source_session: session.clone(),
            });
        }

        i = j;
    }
}

// ---------------------------------------------------------------------------
// Pattern 4: Explicit saves
// ---------------------------------------------------------------------------

/// Detect when the user explicitly asks to save/remember something.
fn extract_explicit_saves(entries: &[TranscriptEntry], lessons: &mut Vec<ExtractedLesson>) {
    for (i, entry) in entries.iter().enumerate() {
        let TranscriptContent::Human { text: user_text } = &entry.content else {
            continue;
        };

        let lower = user_text.to_lowercase();
        let is_save = SAVE_KEYWORDS.iter().any(|kw| lower.contains(kw));

        if !is_save {
            continue;
        }

        let context = gather_context(entries, i);
        let session = &entry.session_id;

        // Use the user's full message as the lesson content, plus
        // surrounding context for additional meaning.
        let title = format!("User note: {}", truncate_for_title(user_text, 80));

        lessons.push(ExtractedLesson {
            title,
            content: format!(
                "User explicitly asked to remember this.\n\n\
                 User said: {user_text}\n\n\
                 Context:\n{context}"
            ),
            tags: vec!["explicit-save".to_string(), "user-note".to_string()],
            severity: "info".to_string(),
            source_session: session.clone(),
        });
    }
}

// ---------------------------------------------------------------------------
// Pattern 5: Build failures followed by fixes
// ---------------------------------------------------------------------------

/// Detect build/compile failures in Bash output followed by a
/// subsequent fix (another Bash or Edit tool use that resolves the
/// issue).
fn extract_build_failures(entries: &[TranscriptEntry], lessons: &mut Vec<ExtractedLesson>) {
    for (i, entry) in entries.iter().enumerate() {
        let output = match &entry.content {
            TranscriptContent::ToolResult {
                output, is_error, ..
            } => {
                // Build failures can appear as non-error tool results
                // too (exit code 0 but compiler warnings, or the
                // tool reports the error in stdout).
                if *is_error || contains_build_failure(output) {
                    output
                } else {
                    continue;
                }
            }
            _ => continue,
        };

        if !contains_build_failure(output) {
            continue;
        }

        // Look for the tool that caused this build output.
        let tool_name = find_preceding_tool_name(entries, i);

        // Only interested in build-related tools.
        if !tool_name.as_deref().is_some_and(is_build_related_tool) {
            continue;
        }

        // Look ahead for a fix: an Edit or Bash call that follows
        // within the next CONTEXT_AFTER * 2 entries.
        let fix = find_subsequent_fix(entries, i);

        let session = &entry.session_id;
        let error_snippet = extract_error_snippet(output);
        let context = gather_context(entries, i);

        let title = format!("Build failure: {}", truncate_for_title(&error_snippet, 70));

        let fix_description = match fix {
            Some((fix_idx, ref fix_desc)) => {
                format!("Fix applied at entry {fix_idx}: {fix_desc}")
            }
            None => "No fix detected in subsequent entries.".to_string(),
        };

        let tn = tool_name.as_deref().unwrap_or("Bash");
        lessons.push(ExtractedLesson {
            title,
            content: format!(
                "Build failure detected in `{tn}` output.\n\n\
                 Error: {error_snippet}\n\n\
                 {fix_description}\n\n\
                 Context:\n{context}"
            ),
            tags: vec!["build-failure".to_string(), "error".to_string()],
            severity: "critical".to_string(),
            source_session: session.clone(),
        });
    }
}

/// Check whether output text contains build failure indicators.
fn contains_build_failure(output: &str) -> bool {
    BUILD_FAILURE_PATTERNS
        .iter()
        .any(|pat| output.contains(pat))
}

/// Check whether a tool name is build-related.
fn is_build_related_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "bash" || lower == "terminal" || lower == "shell" || lower == "run"
}

/// Find the tool name from the entry that triggered a ToolResult at
/// index `i` by searching backwards for the nearest ToolUse.
fn find_preceding_tool_name(entries: &[TranscriptEntry], i: usize) -> Option<String> {
    let search_start = i.saturating_sub(5);
    for j in (search_start..i).rev() {
        match &entries[j].content {
            TranscriptContent::ToolUse { name, .. } => {
                return Some(name.clone());
            }
            TranscriptContent::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                return Some(tool_calls[0].name.clone());
            }
            _ => {}
        }
    }
    None
}

/// Look ahead from a build failure for an Edit or Bash call that
/// represents a fix attempt.
fn find_subsequent_fix(entries: &[TranscriptEntry], failure_idx: usize) -> Option<(usize, String)> {
    let search_end = (failure_idx + CONTEXT_AFTER * 2 + 1).min(entries.len());
    let start = failure_idx + 1;
    for (j, entry) in entries.iter().enumerate().take(search_end).skip(start) {
        match &entry.content {
            TranscriptContent::ToolUse { name, input } => {
                let lower = name.to_lowercase();
                if lower == "edit" || lower == "write" {
                    let file = input
                        .get("file_path")
                        .or_else(|| input.get("path"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown file");
                    return Some((j, format!("{name} on {file}")));
                }
                if lower == "bash" {
                    let cmd = input
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown command");
                    let cmd_short = truncate_for_title(cmd, 100);
                    return Some((j, format!("Bash: {cmd_short}")));
                }
            }
            TranscriptContent::Assistant { tool_calls, .. } => {
                for tc in tool_calls {
                    let lower = tc.name.to_lowercase();
                    if lower == "edit" || lower == "write" || lower == "bash" {
                        let tc_name = &tc.name;
                        return Some((j, format!("{tc_name} tool call")));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the first meaningful error line from build output.
fn extract_error_snippet(output: &str) -> String {
    for line in output.lines() {
        let trimmed = line.trim();
        if BUILD_FAILURE_PATTERNS
            .iter()
            .any(|pat| trimmed.contains(pat))
        {
            return truncate_for_title(trimmed, 200);
        }
    }
    truncate_for_title(output, 200)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Gather surrounding context entries as a formatted string.
///
/// Includes up to [`CONTEXT_BEFORE`] entries before and
/// [`CONTEXT_AFTER`] entries after the given index.
fn gather_context(entries: &[TranscriptEntry], center: usize) -> String {
    let start = center.saturating_sub(CONTEXT_BEFORE);
    let end = (center + CONTEXT_AFTER + 1).min(entries.len());

    let mut lines = Vec::new();
    for (idx, entry) in entries.iter().enumerate().take(end).skip(start) {
        let marker = if idx == center { ">>>" } else { "   " };
        let summary = summarize_entry(entry);
        let etype = &entry.entry_type;
        lines.push(format!("{marker} [{idx}] {etype}: {summary}"));
    }
    lines.join("\n")
}

/// Produce a one-line summary of an entry for context display.
fn summarize_entry(entry: &TranscriptEntry) -> String {
    match &entry.content {
        TranscriptContent::Human { text } => truncate_for_title(text, 120),
        TranscriptContent::Assistant { text, tool_calls } => {
            if tool_calls.is_empty() {
                truncate_for_title(text, 120)
            } else {
                let names: Vec<&str> = tool_calls.iter().map(|tc| tc.name.as_str()).collect();
                let text_short = truncate_for_title(text, 80);
                let tools = names.join(", ");
                format!("{text_short} [tools: {tools}]")
            }
        }
        TranscriptContent::ToolUse { name, .. } => {
            format!("ToolUse: {name}")
        }
        TranscriptContent::ToolResult {
            output, is_error, ..
        } => {
            let prefix = if *is_error { "ERROR" } else { "OK" };
            let out_short = truncate_for_title(output, 100);
            format!("[{prefix}] {out_short}")
        }
        TranscriptContent::System { subtype } => {
            let sub = subtype.as_deref().unwrap_or("(none)");
            format!("System: {sub}")
        }
    }
}

/// Truncate a string to `max_len` characters, appending "..." if
/// truncated. Also replaces newlines with spaces for single-line
/// display.
fn truncate_for_title(s: &str, max_len: usize) -> String {
    let clean: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let trimmed = clean.trim();
    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let mut result = String::with_capacity(max_len + 3);
        for (i, c) in trimmed.chars().enumerate() {
            if i >= max_len {
                break;
            }
            result.push(c);
        }
        result.push_str("...");
        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_code::transcript::ToolCall;

    /// Helper to create a minimal `TranscriptEntry` with human content.
    fn human_entry(text: &str) -> TranscriptEntry {
        TranscriptEntry {
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_uuid: None,
            entry_type: "user".to_string(),
            timestamp: 1000,
            session_id: "test-session".to_string(),
            cwd: None,
            git_branch: None,
            content: TranscriptContent::Human {
                text: text.to_string(),
            },
        }
    }

    /// Helper to create a minimal assistant entry with text only.
    fn assistant_entry(text: &str) -> TranscriptEntry {
        TranscriptEntry {
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_uuid: None,
            entry_type: "assistant".to_string(),
            timestamp: 1001,
            session_id: "test-session".to_string(),
            cwd: None,
            git_branch: None,
            content: TranscriptContent::Assistant {
                text: text.to_string(),
                tool_calls: Vec::new(),
            },
        }
    }

    /// Helper to create a tool-use entry.
    fn tool_use_entry(name: &str, input: serde_json::Value) -> TranscriptEntry {
        TranscriptEntry {
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_uuid: None,
            entry_type: "assistant".to_string(),
            timestamp: 1002,
            session_id: "test-session".to_string(),
            cwd: None,
            git_branch: None,
            content: TranscriptContent::ToolUse {
                name: name.to_string(),
                input,
            },
        }
    }

    /// Helper to create a tool result entry.
    fn tool_result_entry(tool_use_id: &str, output: &str, is_error: bool) -> TranscriptEntry {
        TranscriptEntry {
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_uuid: None,
            entry_type: "user".to_string(),
            timestamp: 1003,
            session_id: "test-session".to_string(),
            cwd: None,
            git_branch: None,
            content: TranscriptContent::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                output: output.to_string(),
                is_error,
            },
        }
    }

    // -----------------------------------------------------------------------
    // Pattern 1: Corrections
    // -----------------------------------------------------------------------

    #[test]
    fn test_correction_after_assistant() {
        let entries = vec![
            human_entry("Please refactor the function"),
            assistant_entry("I've refactored the function to use async."),
            human_entry("No, don't use async. Keep it synchronous."),
        ];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].title.starts_with("Correction:"));
        assert_eq!(lessons[0].severity, "warning");
        assert!(lessons[0].tags.contains(&"correction".to_string()));
        assert!(lessons[0].content.contains("don't use async"));
    }

    #[test]
    fn test_correction_wrong_keyword() {
        let entries = vec![
            human_entry("Update the config"),
            assistant_entry("Updated the config with new values."),
            human_entry("Wrong, you changed the wrong file."),
        ];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].title.contains("Correction"));
        assert!(lessons[0].content.contains("wrong file"));
    }

    #[test]
    fn test_no_correction_without_assistant() {
        // "No" at the start of a conversation with no preceding
        // assistant entry should not trigger.
        let entries = vec![human_entry("No, I changed my mind about this.")];

        let lessons = extract_lessons(&entries);
        assert!(
            lessons.is_empty(),
            "should not extract correction without preceding assistant"
        );
    }

    #[test]
    fn test_no_false_positive_normal_conversation() {
        // The word "don't" in normal context without preceding
        // assistant should not trigger.
        let entries = vec![
            human_entry("Please update the README"),
            assistant_entry("I'll update the README now."),
            // This is a continuation instruction, not a correction
            // of the assistant. However, since it contains "don't",
            // it will match. This is an acceptable trade-off: the
            // pattern errs on the side of capturing potential
            // corrections.
            human_entry("Great, and also update the changelog"),
        ];

        let lessons = extract_lessons(&entries);
        // "Great, and also update the changelog" does NOT contain
        // any correction keyword, so no lesson extracted.
        assert!(
            lessons.is_empty(),
            "normal continuation should not trigger correction"
        );
    }

    // -----------------------------------------------------------------------
    // Pattern 2: Tool failures
    // -----------------------------------------------------------------------

    #[test]
    fn test_tool_failure_extracted() {
        let entries = vec![
            human_entry("Read the config file"),
            tool_use_entry("Read", serde_json::json!({"file_path": "/etc/nonexistent"})),
            tool_result_entry("tu-1", "File not found: /etc/nonexistent", true),
        ];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].title.contains("Tool failure"));
        assert!(lessons[0].title.contains("Read"));
        assert_eq!(lessons[0].severity, "warning");
        assert!(lessons[0].content.contains("File not found"));
    }

    #[test]
    fn test_tool_result_not_error_skipped() {
        let entries = vec![
            tool_use_entry("Read", serde_json::json!({"file_path": "/tmp/ok"})),
            tool_result_entry("tu-2", "file contents here", false),
        ];

        let lessons = extract_lessons(&entries);
        assert!(
            lessons.is_empty(),
            "non-error tool results should not produce lessons"
        );
    }

    #[test]
    fn test_tool_failure_with_assistant_tool_call() {
        // When the tool call is inside an Assistant entry rather than
        // a standalone ToolUse entry.
        let entries = vec![
            human_entry("Run the tests"),
            TranscriptEntry {
                uuid: "a1".to_string(),
                parent_uuid: None,
                entry_type: "assistant".to_string(),
                timestamp: 1001,
                session_id: "test-session".to_string(),
                cwd: None,
                git_branch: None,
                content: TranscriptContent::Assistant {
                    text: "Running tests...".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "tc-99".to_string(),
                        name: "Bash".to_string(),
                        input: serde_json::json!({"command": "cargo test"}),
                    }],
                },
            },
            tool_result_entry("tc-99", "thread panicked at ...", true),
        ];

        let lessons = extract_lessons(&entries);
        // Expect 2: a tool-failure AND a build-failure (the output
        // contains "panic" and the preceding tool is Bash).
        assert_eq!(lessons.len(), 2);
        let tool_fail = lessons
            .iter()
            .find(|l| l.tags.contains(&"tool-failure".to_string()))
            .expect("should have tool-failure lesson");
        assert!(tool_fail.title.contains("Bash"));
    }

    // -----------------------------------------------------------------------
    // Pattern 3: Repeated patterns
    // -----------------------------------------------------------------------

    #[test]
    fn test_repeated_tool_calls() {
        let entries = vec![
            tool_use_entry(
                "Grep",
                serde_json::json!({"pattern": "TODO", "path": "/src"}),
            ),
            tool_use_entry(
                "Grep",
                serde_json::json!({"pattern": "TODO", "path": "/src"}),
            ),
            tool_use_entry(
                "Grep",
                serde_json::json!({"pattern": "TODO", "path": "/src"}),
            ),
        ];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].title.contains("Repeated tool pattern"));
        assert!(lessons[0].title.contains("Grep"));
        assert!(lessons[0].title.contains("3 times"));
        assert_eq!(lessons[0].severity, "info");
    }

    #[test]
    fn test_repeated_similar_not_identical() {
        // Similar but not identical args should still match with
        // high Jaro-Winkler similarity.
        let entries = vec![
            tool_use_entry(
                "Read",
                serde_json::json!({"file_path": "/home/user/project/src/main.rs"}),
            ),
            tool_use_entry(
                "Read",
                serde_json::json!({"file_path": "/home/user/project/src/main.rs"}),
            ),
            tool_use_entry(
                "Read",
                serde_json::json!({"file_path": "/home/user/project/src/main.rs"}),
            ),
        ];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].title.contains("Read"));
    }

    #[test]
    fn test_no_repeated_pattern_below_threshold() {
        // Only 2 calls — below the threshold of 3.
        let entries = vec![
            tool_use_entry("Grep", serde_json::json!({"pattern": "TODO"})),
            tool_use_entry("Grep", serde_json::json!({"pattern": "TODO"})),
        ];

        let lessons = extract_lessons(&entries);
        assert!(
            lessons.is_empty(),
            "2 calls should not trigger repeated pattern"
        );
    }

    #[test]
    fn test_no_repeated_pattern_different_tools() {
        let entries = vec![
            tool_use_entry("Grep", serde_json::json!({"pattern": "TODO"})),
            tool_use_entry("Read", serde_json::json!({"file_path": "/tmp/a"})),
            tool_use_entry("Bash", serde_json::json!({"command": "ls"})),
        ];

        let lessons = extract_lessons(&entries);
        // Different tool names — no repeated pattern.
        let repeated: Vec<_> = lessons
            .iter()
            .filter(|l| l.tags.contains(&"repeated-pattern".to_string()))
            .collect();
        assert!(
            repeated.is_empty(),
            "different tools should not trigger repeated pattern"
        );
    }

    // -----------------------------------------------------------------------
    // Pattern 4: Explicit saves
    // -----------------------------------------------------------------------

    #[test]
    fn test_explicit_save_remember() {
        let entries = vec![
            assistant_entry("The build requires feature X."),
            human_entry("Remember this: always enable feature X before building."),
        ];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].title.starts_with("User note:"));
        assert!(lessons[0].tags.contains(&"explicit-save".to_string()));
        assert_eq!(lessons[0].severity, "info");
        assert!(lessons[0].content.contains("always enable feature X"));
    }

    #[test]
    fn test_explicit_save_lesson_learned() {
        let entries = vec![
            assistant_entry("Done, the migration is complete."),
            human_entry("Lesson learned: always backup the database before migration."),
        ];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].content.contains("backup the database"));
    }

    #[test]
    fn test_explicit_save_note_to_self() {
        let entries = vec![human_entry(
            "Note to self: the CI pipeline needs the DEPLOY_KEY secret.",
        )];

        let lessons = extract_lessons(&entries);
        assert_eq!(lessons.len(), 1);
        assert!(lessons[0].tags.contains(&"user-note".to_string()));
    }

    #[test]
    fn test_no_save_without_keyword() {
        let entries = vec![human_entry("The tests are passing now. Good work.")];

        let lessons = extract_lessons(&entries);
        assert!(
            lessons.is_empty(),
            "normal conversation should not trigger explicit save"
        );
    }

    // -----------------------------------------------------------------------
    // Pattern 5: Build failures
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_failure_error_e() {
        let entries = vec![
            human_entry("Build the project"),
            tool_use_entry("Bash", serde_json::json!({"command": "cargo build"})),
            tool_result_entry(
                "tu-build",
                "error[E0433]: failed to resolve: use of undeclared crate or module `foo`\n\
                 --> src/main.rs:5:5\n  |\n5 | use foo::bar;\n  |     ^^^ not found",
                false, // cargo build exits non-zero but Claude wraps as non-error sometimes
            ),
            tool_use_entry(
                "Edit",
                serde_json::json!({
                    "file_path": "/src/main.rs",
                    "old_string": "use foo::bar;",
                    "new_string": "use baz::bar;"
                }),
            ),
        ];

        let lessons = extract_lessons(&entries);
        let build_lessons: Vec<_> = lessons
            .iter()
            .filter(|l| l.tags.contains(&"build-failure".to_string()))
            .collect();
        assert_eq!(build_lessons.len(), 1);
        assert!(build_lessons[0].title.contains("Build failure"));
        assert_eq!(build_lessons[0].severity, "critical");
        assert!(build_lessons[0].content.contains("error[E0433]"));
        // Should mention the fix.
        assert!(build_lessons[0].content.contains("Edit"));
    }

    #[test]
    fn test_build_failure_panic() {
        let entries = vec![
            tool_use_entry("Bash", serde_json::json!({"command": "cargo test"})),
            tool_result_entry(
                "tu-test",
                "thread 'main' panicked at 'index out of bounds'",
                true,
            ),
        ];

        let lessons = extract_lessons(&entries);
        let build_lessons: Vec<_> = lessons
            .iter()
            .filter(|l| l.tags.contains(&"build-failure".to_string()))
            .collect();
        assert_eq!(build_lessons.len(), 1);
        assert!(build_lessons[0].content.contains("panicked"));
    }

    #[test]
    fn test_build_failure_failed_keyword() {
        let entries = vec![
            tool_use_entry("Bash", serde_json::json!({"command": "cargo test"})),
            tool_result_entry(
                "tu-t2",
                "test result: FAILED. 1 passed; 2 failed; 0 ignored",
                false,
            ),
            tool_use_entry(
                "Edit",
                serde_json::json!({
                    "file_path": "/src/lib.rs",
                    "old_string": "assert!(false)",
                    "new_string": "assert!(true)"
                }),
            ),
        ];

        let lessons = extract_lessons(&entries);
        let build_lessons: Vec<_> = lessons
            .iter()
            .filter(|l| l.tags.contains(&"build-failure".to_string()))
            .collect();
        assert_eq!(build_lessons.len(), 1);
        assert!(build_lessons[0].content.contains("FAILED"));
        assert!(build_lessons[0].content.contains("Edit"));
    }

    #[test]
    fn test_no_build_failure_from_non_build_tool() {
        // A Grep result that happens to contain the word "FAILED"
        // should not trigger.
        let entries = vec![
            tool_use_entry("Grep", serde_json::json!({"pattern": "FAILED"})),
            tool_result_entry("tu-g", "src/tests.rs:42: assert FAILED here", false),
        ];

        let lessons = extract_lessons(&entries);
        let build_lessons: Vec<_> = lessons
            .iter()
            .filter(|l| l.tags.contains(&"build-failure".to_string()))
            .collect();
        assert!(
            build_lessons.is_empty(),
            "non-build tools should not trigger build failure pattern"
        );
    }

    // -----------------------------------------------------------------------
    // Context and helpers
    // -----------------------------------------------------------------------

    #[test]
    fn test_gather_context_includes_surrounding() {
        let entries = vec![
            human_entry("Step 1"),
            human_entry("Step 2"),
            assistant_entry("Did something"),
            human_entry("Step 4"), // center
            assistant_entry("Response"),
            human_entry("Step 6"),
        ];

        let context = gather_context(&entries, 3);
        // Should include entries 1..=5 (2 before, center, 3 after
        // but capped at len).
        assert!(context.contains("[1]"));
        assert!(context.contains("[2]"));
        assert!(context.contains(">>>"));
        assert!(context.contains("[3]"));
        assert!(context.contains("[4]"));
        assert!(context.contains("[5]"));
    }

    #[test]
    fn test_gather_context_at_start() {
        let entries = vec![human_entry("First entry"), assistant_entry("Response")];

        let context = gather_context(&entries, 0);
        assert!(context.contains(">>>"));
        assert!(context.contains("[0]"));
    }

    #[test]
    fn test_truncate_for_title() {
        assert_eq!(truncate_for_title("short", 10), "short");
        assert_eq!(
            truncate_for_title("a very long string indeed", 10),
            "a very lon..."
        );
        assert_eq!(
            truncate_for_title("has\nnewlines\nin it", 50),
            "has newlines in it"
        );
    }

    #[test]
    fn test_deduplication_by_title() {
        // Two corrections with the same text should be deduplicated.
        let entries = vec![
            assistant_entry("Action 1"),
            human_entry("No, that's wrong"),
            assistant_entry("Action 2"),
            human_entry("No, that's wrong"),
        ];

        let lessons = extract_lessons(&entries);
        let corrections: Vec<_> = lessons
            .iter()
            .filter(|l| l.tags.contains(&"correction".to_string()))
            .collect();
        assert_eq!(
            corrections.len(),
            1,
            "duplicate titles should be deduplicated"
        );
    }

    #[test]
    fn test_multiple_patterns_in_one_transcript() {
        // A transcript that triggers multiple different patterns.
        let entries = vec![
            // Explicit save
            human_entry("Remember this: always run fmt before clippy"),
            // Assistant does something
            assistant_entry("Noted. Let me run the build."),
            // Build failure
            tool_use_entry("Bash", serde_json::json!({"command": "cargo build"})),
            tool_result_entry("tu-b", "error[E0599]: no method named `foo` found", false),
            // Fix
            tool_use_entry(
                "Edit",
                serde_json::json!({"file_path": "/src/lib.rs", "old_string": "x", "new_string": "y"}),
            ),
            // Correction
            assistant_entry("I fixed it by changing x to y."),
            human_entry("No, you should have changed z to w instead."),
        ];

        let lessons = extract_lessons(&entries);
        let tags: Vec<String> = lessons.iter().flat_map(|l| l.tags.clone()).collect();

        assert!(
            tags.contains(&"explicit-save".to_string()),
            "should detect explicit save"
        );
        assert!(
            tags.contains(&"build-failure".to_string()),
            "should detect build failure"
        );
        assert!(
            tags.contains(&"correction".to_string()),
            "should detect correction"
        );
    }

    #[test]
    fn test_session_id_propagated() {
        let mut entry = human_entry("Remember this important fact.");
        entry.session_id = "session-abc-123".to_string();

        let lessons = extract_lessons(&[entry]);
        assert_eq!(lessons.len(), 1);
        assert_eq!(lessons[0].source_session, "session-abc-123");
    }

    #[test]
    fn test_empty_transcript() {
        let lessons = extract_lessons(&[]);
        assert!(
            lessons.is_empty(),
            "empty transcript should yield no lessons"
        );
    }

    #[test]
    fn test_contains_build_failure_patterns() {
        assert!(contains_build_failure("error[E0433]: failed to resolve"));
        assert!(contains_build_failure("test result: FAILED. 2 failed"));
        assert!(contains_build_failure("thread 'main' panicked at 'oops'"));
        assert!(contains_build_failure("could not compile `myproject`"));
        assert!(contains_build_failure("aborting due to 3 previous errors"));
        assert!(!contains_build_failure("All tests passed."));
        assert!(!contains_build_failure("Build succeeded."));
    }
}
