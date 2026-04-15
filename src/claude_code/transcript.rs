//! JSONL transcript parser for Claude Code sessions.
//!
//! Claude Code writes session transcripts as JSONL files in
//! `~/.claude/projects/<project>/<session-id>.jsonl`. Each line is a
//! JSON object representing one event in the conversation: user
//! messages, assistant responses, tool uses, tool results, system
//! events, and progress updates.
//!
//! This module parses those files into structured [`TranscriptEntry`]
//! values suitable for downstream pattern extraction and lesson
//! mining.
//!
//! # Resilience
//!
//! Malformed lines are skipped with a `tracing::warn` log rather
//! than failing the entire parse, because real transcripts sometimes
//! contain partial writes or non-standard event types.
//!
//! # Examples
//!
//! ```rust,ignore
//! use std::path::Path;
//! use nellie::claude_code::transcript::parse_transcript;
//!
//! let entries = parse_transcript(Path::new("session.jsonl"))?;
//! for entry in &entries {
//!     println!("{}: {:?}", entry.entry_type, entry.content);
//! }
//! ```

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Errors that can occur during transcript parsing.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    /// Failed to read the transcript file from disk.
    #[error("failed to read transcript file: {0}")]
    Io(#[from] std::io::Error),
}

/// A single parsed entry from a Claude Code session transcript.
///
/// Each entry corresponds to one JSONL line from the transcript file.
/// Only conversationally meaningful types (`user`, `assistant`,
/// `system`) are parsed; housekeeping types (`progress`,
/// `file-history-snapshot`, `queue-operation`) are skipped during
/// parsing.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Unique identifier for this entry.
    pub uuid: String,
    /// UUID of the parent entry in the conversation tree, if any.
    pub parent_uuid: Option<String>,
    /// Entry type from the raw JSONL: `"user"`, `"assistant"`, or
    /// `"system"`.
    pub entry_type: String,
    /// Unix timestamp (millisecond-precision ISO 8601 string from
    /// the transcript, converted to epoch seconds).
    pub timestamp: i64,
    /// Session identifier grouping entries from the same conversation.
    pub session_id: String,
    /// Working directory at the time of this entry.
    pub cwd: Option<String>,
    /// Git branch active at the time of this entry.
    pub git_branch: Option<String>,
    /// Parsed content of the entry, typed by role.
    pub content: TranscriptContent,
}

/// Typed content for a transcript entry.
///
/// The variant is determined by the combination of the top-level
/// `type` field and the `message.content` structure in the raw
/// JSONL.
#[derive(Debug, Clone)]
pub enum TranscriptContent {
    /// A message from the human user (plain text).
    Human {
        /// The user's message text.
        text: String,
    },
    /// An assistant response, which may contain text and/or tool
    /// calls.
    Assistant {
        /// Plain text portions of the response (concatenated).
        text: String,
        /// Tool calls made in this response.
        tool_calls: Vec<ToolCall>,
    },
    /// A tool use request from the assistant. This is a separate
    /// variant because Claude Code emits individual tool_use content
    /// blocks as their own JSONL lines in some cases.
    ToolUse {
        /// Tool name (e.g., `"Read"`, `"Bash"`, `"Edit"`).
        name: String,
        /// Tool input as opaque JSON.
        input: serde_json::Value,
    },
    /// The result of a tool execution, sent back to the assistant.
    ToolResult {
        /// ID of the corresponding tool_use block.
        tool_use_id: String,
        /// Output text from the tool.
        output: String,
        /// Whether the tool execution resulted in an error.
        is_error: bool,
    },
    /// A system event (e.g., session metadata, duration marker).
    System {
        /// Subtype of the system event, if present.
        subtype: Option<String>,
    },
}

/// A tool call extracted from an assistant message.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Unique identifier for this tool use (matches `tool_use_id` in
    /// the corresponding `ToolResult`).
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool input as opaque JSON.
    pub input: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Raw deserialization types (internal)
// ---------------------------------------------------------------------------

/// Raw top-level JSONL line before semantic interpretation.
#[derive(Debug, Deserialize)]
struct RawEntry {
    uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    parent_uuid: Option<String>,
    #[serde(rename = "type")]
    entry_type: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    message: Option<RawMessage>,
    /// Claude Code native format uses `data` field instead of `message`
    data: Option<RawData>,
    /// System events carry a subtype field.
    subtype: Option<String>,
    /// Tool use ID (for tool_use entries in data format)
    #[serde(default)]
    tool_use_id: Option<String>,
    /// Tool name (for tool_use entries in data format)
    #[serde(default)]
    name: Option<String>,
    /// Tool input (for tool_use entries in data format)
    #[serde(default)]
    input: Option<serde_json::Value>,
}

/// The `data` object used in Claude Code native format (modern format).
#[derive(Debug, Deserialize)]
struct RawData {
    /// Human text (for human/assistant entries)
    text: Option<String>,
    /// Tool name (for tool_use entries)
    name: Option<String>,
    /// Tool input (for tool_use entries)
    input: Option<serde_json::Value>,
    /// Tool use ID (for tool_result entries)
    #[serde(rename = "toolUseID")]
    tool_use_id: Option<String>,
    /// Tool result output (for tool_result entries)
    output: Option<String>,
    /// Whether tool result is an error (for tool_result entries)
    is_error: Option<bool>,
    /// Tool calls array (for assistant entries with toolCalls)
    #[serde(default)]
    #[allow(dead_code)]
    tool_calls: Option<Vec<serde_json::Value>>,
}

/// The `message` object nested inside a transcript line (legacy format).
#[derive(Debug, Deserialize)]
struct RawMessage {
    /// Kept for deserialization fidelity; used in debug output only.
    #[allow(dead_code)]
    role: Option<String>,
    content: Option<RawContent>,
}

/// Message content is either a plain string or an array of content
/// blocks.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawContent {
    Text(String),
    Blocks(Vec<RawContentBlock>),
}

/// A single content block inside a message.
#[derive(Debug, Deserialize)]
struct RawContentBlock {
    #[serde(rename = "type")]
    block_type: Option<String>,
    /// Text content (for `"text"` blocks).
    text: Option<String>,
    /// Thinking content (for `"thinking"` blocks — ignored during
    /// extraction since it is internal reasoning).
    #[serde(rename = "thinking")]
    _thinking: Option<String>,
    // -- tool_use fields --
    /// Tool use ID.
    id: Option<String>,
    /// Tool name.
    name: Option<String>,
    /// Tool input.
    input: Option<serde_json::Value>,
    // -- tool_result fields --
    /// Corresponding tool_use ID for result blocks.
    tool_use_id: Option<String>,
    /// Tool result content (can be string or structured).
    content: Option<serde_json::Value>,
    /// Whether this tool result is an error.
    is_error: Option<bool>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a Claude Code JSONL transcript file into structured entries.
///
/// Lines that fail to parse or represent non-conversational event
/// types (`progress`, `file-history-snapshot`, `queue-operation`) are
/// silently skipped (with a `tracing::warn` for parse failures).
///
/// # Errors
///
/// Returns [`TranscriptError::Io`] if the file cannot be read.
pub fn parse_transcript(path: &Path) -> Result<Vec<TranscriptEntry>, TranscriptError> {
    let contents = fs::read_to_string(path)?;
    Ok(parse_transcript_str(&contents))
}

/// Parse a JSONL transcript from an in-memory string.
///
/// This is the workhorse function, separated from I/O for testability.
pub fn parse_transcript_str(contents: &str) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();

    for (line_num, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let raw: RawEntry = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    line_num = line_num + 1,
                    error = %e,
                    "skipping malformed transcript line"
                );
                continue;
            }
        };

        // Normalize entry type: map modern Claude Code format to canonical types
        // Modern format uses "human", "assistant", "tool_use", "tool_result"
        // Legacy format uses "user", "assistant", "system"
        let entry_type = match raw.entry_type.as_deref() {
            Some("human") => "user".to_string(),
            Some("user" | "assistant" | "system") => raw.entry_type.clone().unwrap_or_default(),
            Some("tool_use") => {
                // Tool use is treated as a special assistant variant
                "tool_use".to_string()
            }
            Some("tool_result") => {
                // Tool result is treated as a special user variant
                "tool_result".to_string()
            }
            Some(other) => {
                tracing::trace!(
                    line_num = line_num + 1,
                    entry_type = other,
                    "skipping non-conversational entry type"
                );
                continue;
            }
            None => continue,
        };

        let uuid = match raw.uuid {
            Some(ref u) => u.clone(),
            None => continue,
        };

        let timestamp = raw
            .timestamp
            .as_deref()
            .and_then(parse_iso8601_to_epoch)
            .unwrap_or(0);

        let session_id = raw.session_id.clone().unwrap_or_default();

        let content = interpret_content(&entry_type, &raw);

        entries.push(TranscriptEntry {
            uuid,
            parent_uuid: raw.parent_uuid.clone(),
            entry_type,
            timestamp,
            session_id,
            cwd: raw.cwd.clone(),
            git_branch: raw.git_branch.clone(),
            content,
        });
    }

    entries
}

// ---------------------------------------------------------------------------
// Content interpretation
// ---------------------------------------------------------------------------

/// Convert the raw entry into a typed [`TranscriptContent`].
fn interpret_content(entry_type: &str, raw: &RawEntry) -> TranscriptContent {
    match entry_type {
        "user" => interpret_user_content(raw),
        "assistant" => interpret_assistant_content(raw),
        "tool_use" => interpret_tool_use_content(raw),
        "tool_result" => interpret_tool_result_content(raw),
        "system" => TranscriptContent::System {
            subtype: raw.subtype.clone(),
        },
        _ => TranscriptContent::System { subtype: None },
    }
}

/// Interpret a `user` entry. User entries can be plain text messages
/// or tool results (which Claude Code wraps in the `user` role).
fn interpret_user_content(raw: &RawEntry) -> TranscriptContent {
    // Try modern format (data field) first
    if let Some(ref data) = raw.data {
        if let Some(ref text) = data.text {
            return TranscriptContent::Human { text: text.clone() };
        }
    }

    // Fall back to legacy format (message field)
    let Some(ref msg) = raw.message else {
        return TranscriptContent::Human {
            text: String::new(),
        };
    };

    match msg.content {
        Some(RawContent::Text(ref t)) => TranscriptContent::Human { text: t.clone() },
        Some(RawContent::Blocks(ref blocks)) => {
            // Check if any block is a tool_result.
            for block in blocks {
                if block.block_type.as_deref() == Some("tool_result") {
                    let output = block
                        .content
                        .as_ref()
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    return TranscriptContent::ToolResult {
                        tool_use_id: block.tool_use_id.clone().unwrap_or_default(),
                        output,
                        is_error: block.is_error.unwrap_or(false),
                    };
                }
            }
            // Otherwise concatenate text blocks.
            let text = blocks
                .iter()
                .filter(|b| b.block_type.as_deref() == Some("text"))
                .filter_map(|b| b.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            TranscriptContent::Human { text }
        }
        None => TranscriptContent::Human {
            text: String::new(),
        },
    }
}

/// Interpret an `assistant` entry. Assistant entries contain a mix
/// of text blocks, thinking blocks (ignored), and tool_use blocks.
fn interpret_assistant_content(raw: &RawEntry) -> TranscriptContent {
    // Try modern format (data field) first
    if let Some(ref data) = raw.data {
        if let Some(ref text) = data.text {
            return TranscriptContent::Assistant {
                text: text.clone(),
                tool_calls: Vec::new(),
            };
        }
    }

    // Fall back to legacy format (message field)
    let Some(ref msg) = raw.message else {
        return TranscriptContent::Assistant {
            text: String::new(),
            tool_calls: Vec::new(),
        };
    };

    match msg.content {
        Some(RawContent::Text(ref t)) => TranscriptContent::Assistant {
            text: t.clone(),
            tool_calls: Vec::new(),
        },
        Some(RawContent::Blocks(ref blocks)) => {
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();

            for block in blocks {
                match block.block_type.as_deref() {
                    Some("text") => {
                        if let Some(ref t) = block.text {
                            text_parts.push(t.as_str());
                        }
                    }
                    Some("tool_use") => {
                        // If the assistant line contains *only* a
                        // tool_use block (very common in Claude Code
                        // transcripts), we return a ToolUse variant
                        // instead of Assistant — but only if there
                        // are no text blocks.
                        tool_calls.push(ToolCall {
                            id: block.id.clone().unwrap_or_default(),
                            name: block.name.clone().unwrap_or_default(),
                            input: block.input.clone().unwrap_or(serde_json::Value::Null),
                        });
                    }
                    // Skip thinking, signature, and other blocks.
                    _ => {}
                }
            }

            // If there are only tool calls and no text, emit a
            // ToolUse for the first one (matches plan's enum).
            // This makes downstream extraction simpler.
            if text_parts.is_empty() && tool_calls.len() == 1 {
                let tc = tool_calls.remove(0);
                return TranscriptContent::ToolUse {
                    name: tc.name,
                    input: tc.input,
                };
            }

            TranscriptContent::Assistant {
                text: text_parts.join("\n"),
                tool_calls,
            }
        }
        None => TranscriptContent::Assistant {
            text: String::new(),
            tool_calls: Vec::new(),
        },
    }
}

/// Interpret a `tool_use` entry in the modern Claude Code format.
/// This is emitted as a standalone entry rather than nested in an assistant message.
fn interpret_tool_use_content(raw: &RawEntry) -> TranscriptContent {
    // Try to extract from `data` field first (modern format)
    if let Some(ref data) = raw.data {
        if let (Some(name), Some(input)) = (data.name.as_ref(), data.input.as_ref()) {
            return TranscriptContent::ToolUse {
                name: name.clone(),
                input: input.clone(),
            };
        }
    }

    // Fallback to top-level fields
    if let (Some(name), input) = (raw.name.as_ref(), raw.input.as_ref()) {
        return TranscriptContent::ToolUse {
            name: name.clone(),
            input: input.cloned().unwrap_or(serde_json::Value::Null),
        };
    }

    TranscriptContent::ToolUse {
        name: String::new(),
        input: serde_json::Value::Null,
    }
}

/// Interpret a `tool_result` entry in the modern Claude Code format.
/// This is emitted as a standalone entry rather than nested in a user message.
fn interpret_tool_result_content(raw: &RawEntry) -> TranscriptContent {
    // Try to extract from `data` field first (modern format)
    if let Some(ref data) = raw.data {
        let output = data.output.clone().unwrap_or_default();
        let tool_use_id = data.tool_use_id.clone().unwrap_or_default();
        let is_error = data.is_error.unwrap_or(false);
        return TranscriptContent::ToolResult {
            tool_use_id,
            output,
            is_error,
        };
    }

    // Fallback to top-level fields
    let output = String::new();
    let tool_use_id = raw.tool_use_id.clone().unwrap_or_default();
    let is_error = false;

    TranscriptContent::ToolResult {
        tool_use_id,
        output,
        is_error,
    }
}

// ---------------------------------------------------------------------------
// Timestamp parsing
// ---------------------------------------------------------------------------

/// Parse an ISO 8601 timestamp string to Unix epoch seconds.
///
/// Handles the format emitted by Claude Code:
/// `"2026-03-02T03:11:24.652Z"` (always UTC, always with
/// milliseconds).
///
/// Returns `None` for unparseable strings rather than failing.
fn parse_iso8601_to_epoch(s: &str) -> Option<i64> {
    // Strip trailing 'Z' and split on 'T'.
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix('z'))?;
    let (date_part, time_part) = s.split_once('T')?;

    let mut date_iter = date_part.splitn(3, '-');
    let year: i64 = date_iter.next()?.parse().ok()?;
    let month: u32 = date_iter.next()?.parse().ok()?;
    let day: u32 = date_iter.next()?.parse().ok()?;

    // Split time on '.' to separate seconds from fractional part.
    let time_no_frac = time_part.split('.').next()?;
    let mut time_iter = time_no_frac.splitn(3, ':');
    let hour: u32 = time_iter.next()?.parse().ok()?;
    let min: u32 = time_iter.next()?.parse().ok()?;
    let sec: u32 = time_iter.next()?.parse().ok()?;

    // Convert to epoch using civil-day algorithm (Howard Hinnant).
    let epoch_days = civil_days_from_civil(year, month, day);
    let epoch_secs =
        epoch_days * 86400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
    Some(epoch_secs)
}

/// Days since Unix epoch for a civil date (Howard Hinnant's
/// algorithm). Handles dates from year 0 to far future correctly.
fn civil_days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = month;
    let doy = if m > 2 {
        (153 * (m - 3) + 2) / 5 + day - 1
    } else {
        (153 * (m + 9) + 2) / 5 + day - 1
    };
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Timestamp parsing tests --

    #[test]
    fn test_parse_iso8601_basic() {
        let ts = parse_iso8601_to_epoch("2026-03-02T03:11:24.652Z");
        assert!(ts.is_some());
        let epoch = ts.unwrap();
        // 2026-03-02T03:11:24 UTC = 1772421084
        assert_eq!(epoch, 1_772_421_084);
    }

    #[test]
    fn test_parse_iso8601_no_millis() {
        let ts = parse_iso8601_to_epoch("2025-01-01T00:00:00Z");
        assert_eq!(ts, Some(1_735_689_600));
    }

    #[test]
    fn test_parse_iso8601_invalid() {
        assert_eq!(parse_iso8601_to_epoch("not-a-date"), None);
        assert_eq!(parse_iso8601_to_epoch(""), None);
        assert_eq!(parse_iso8601_to_epoch("2025-01-01"), None);
    }

    #[test]
    fn test_parse_iso8601_lowercase_z() {
        let ts = parse_iso8601_to_epoch("2025-06-15T12:30:00.000z");
        assert!(ts.is_some());
    }

    // -- Civil days algorithm --

    #[test]
    fn test_civil_days_epoch() {
        // 1970-01-01 should be day 0.
        assert_eq!(civil_days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn test_civil_days_known_date() {
        // 2000-01-01 = day 10957
        assert_eq!(civil_days_from_civil(2000, 1, 1), 10_957);
    }

    // -- JSONL parsing: human messages --

    #[test]
    fn test_parse_human_text_content() {
        let jsonl = r#"{"parentUuid":null,"cwd":"/home/user/project","sessionId":"sess-1","version":"2.1.63","gitBranch":"main","type":"user","message":{"role":"user","content":"Hello world"},"uuid":"uuid-1","timestamp":"2026-03-02T03:11:24.652Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);

        let e = &entries[0];
        assert_eq!(e.uuid, "uuid-1");
        assert_eq!(e.entry_type, "user");
        assert_eq!(e.session_id, "sess-1");
        assert_eq!(e.cwd.as_deref(), Some("/home/user/project"));
        assert_eq!(e.git_branch.as_deref(), Some("main"));
        assert!(e.timestamp > 0);

        match &e.content {
            TranscriptContent::Human { text } => {
                assert_eq!(text, "Hello world");
            }
            other => panic!("expected Human, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_human_content_blocks() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"block text"}]},"uuid":"uuid-2","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Human { text } => {
                assert_eq!(text, "block text");
            }
            other => panic!("expected Human, got {:?}", other),
        }
    }

    // -- JSONL parsing: assistant messages --

    #[test]
    fn test_parse_assistant_text() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Here is my response."}]},"uuid":"uuid-3","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Assistant { text, tool_calls } => {
                assert_eq!(text, "Here is my response.");
                assert!(tool_calls.is_empty());
            }
            other => panic!("expected Assistant, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_assistant_with_tool_calls() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"tool-1","name":"Read","input":{"file_path":"/tmp/foo"}}]},"uuid":"uuid-4","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Assistant { text, tool_calls } => {
                assert_eq!(text, "Let me check.");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "tool-1");
                assert_eq!(tool_calls[0].name, "Read");
            }
            other => panic!("expected Assistant, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_assistant_thinking_ignored() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"I am pondering...","signature":"sig123"}]},"uuid":"uuid-5","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Assistant { text, tool_calls } => {
                // Thinking blocks are ignored — no text extracted.
                assert!(text.is_empty());
                assert!(tool_calls.is_empty());
            }
            other => panic!("expected Assistant, got {:?}", other),
        }
    }

    // -- JSONL parsing: tool_use (standalone) --

    #[test]
    fn test_parse_standalone_tool_use() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu-abc","name":"Bash","input":{"command":"ls -la"}}]},"uuid":"uuid-6","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::ToolUse { name, input } => {
                assert_eq!(name, "Bash");
                assert_eq!(input["command"], "ls -la");
            }
            other => panic!("expected ToolUse, got {:?}", other),
        }
    }

    // -- JSONL parsing: tool_result --

    #[test]
    fn test_parse_tool_result() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu-abc","content":"file contents here","is_error":false}]},"uuid":"uuid-7","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => {
                assert_eq!(tool_use_id, "toolu-abc");
                assert_eq!(output, "file contents here");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_tool_result_error() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu-err","content":"Error: file not found","is_error":true}]},"uuid":"uuid-8","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::ToolResult {
                is_error, output, ..
            } => {
                assert!(is_error);
                assert!(output.contains("file not found"));
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_tool_result_structured_content() {
        // Some tool results have JSON content rather than string.
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu-x","content":{"key":"value"}}]},"uuid":"uuid-9","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::ToolResult { output, .. } => {
                assert!(output.contains("key"));
                assert!(output.contains("value"));
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    // -- JSONL parsing: system entries --

    #[test]
    fn test_parse_system_entry() {
        let jsonl = r#"{"type":"system","subtype":"duration","uuid":"uuid-sys","sessionId":"s","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, "system");
        match &entries[0].content {
            TranscriptContent::System { subtype } => {
                assert_eq!(subtype.as_deref(), Some("duration"));
            }
            other => panic!("expected System, got {:?}", other),
        }
    }

    // -- Skipped types --

    #[test]
    fn test_skip_progress_entries() {
        let jsonl = r#"{"type":"progress","data":{"type":"hook_progress"},"uuid":"uuid-p","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_skip_file_history_snapshot() {
        let jsonl = r#"{"type":"file-history-snapshot","messageId":"m","snapshot":{"messageId":"m","trackedFileBackups":{},"timestamp":"2025-01-01T00:00:00Z"},"isSnapshotUpdate":false}"#;

        let entries = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_skip_queue_operation() {
        let jsonl = r#"{"type":"queue-operation","uuid":"uuid-q","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    // -- Malformed / edge cases --

    #[test]
    fn test_malformed_line_skipped() {
        let jsonl = "this is not json\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"},\"uuid\":\"uuid-ok\",\"sessionId\":\"s\",\"timestamp\":\"2025-01-01T00:00:00Z\"}";

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].uuid, "uuid-ok");
    }

    #[test]
    fn test_empty_lines_skipped() {
        let jsonl = "\n\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"},\"uuid\":\"uuid-e\",\"sessionId\":\"s\",\"timestamp\":\"2025-01-01T00:00:00Z\"}\n\n";

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_missing_uuid_skipped() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"no uuid"},"sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_missing_timestamp_defaults_to_zero() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"no time"},"uuid":"uuid-nt","sessionId":"s"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].timestamp, 0);
    }

    #[test]
    fn test_missing_session_id_defaults_to_empty() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"hi"},"uuid":"uuid-ns","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "");
    }

    #[test]
    fn test_missing_optional_fields() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"minimal"},"uuid":"uuid-min","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].cwd.is_none());
        assert!(entries[0].git_branch.is_none());
    }

    #[test]
    fn test_no_message_field() {
        // A user entry with no message field should still parse
        // (empty Human text).
        let jsonl = r#"{"type":"user","uuid":"uuid-nm","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Human { text } => {
                assert!(text.is_empty());
            }
            other => panic!("expected Human, got {:?}", other),
        }
    }

    // -- Multi-line transcript --

    #[test]
    fn test_multi_line_transcript() {
        let jsonl = [
            r#"{"type":"user","message":{"role":"user","content":"What is 2+2?"},"uuid":"u1","sessionId":"s","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp","gitBranch":"main"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"The answer is 4."}]},"uuid":"u2","parentUuid":"u1","sessionId":"s","timestamp":"2025-01-01T00:00:01Z"}"#,
            r#"{"type":"progress","data":{},"uuid":"u3","sessionId":"s","timestamp":"2025-01-01T00:00:01Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":"Thanks!"},"uuid":"u4","parentUuid":"u2","sessionId":"s","timestamp":"2025-01-01T00:00:02Z"}"#,
        ]
        .join("\n");

        let entries = parse_transcript_str(&jsonl);
        assert_eq!(entries.len(), 3); // progress skipped

        assert_eq!(entries[0].entry_type, "user");
        assert_eq!(entries[1].entry_type, "assistant");
        assert_eq!(entries[2].entry_type, "user");

        // Check parent chain.
        assert!(entries[0].parent_uuid.is_none());
        assert_eq!(entries[1].parent_uuid.as_deref(), Some("u1"));
        assert_eq!(entries[2].parent_uuid.as_deref(), Some("u2"));
    }

    // -- Real-world format fidelity --

    #[test]
    fn test_real_format_user_entry() {
        // Mirrors the actual Claude Code JSONL format observed.
        let jsonl = r#"{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"/home/mmn/github/nellie-rs","sessionId":"0d74b5ea-5186-4d10-8bda-7820a699573e","version":"2.1.63","gitBranch":"main","type":"user","message":{"role":"user","content":"have we started work on the dev plan v plan yet?"},"uuid":"7886c22d-835e-41a9-86fa-6f28c872ec3e","timestamp":"2026-03-02T03:11:24.649Z","permissionMode":"bypassPermissions"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);

        let e = &entries[0];
        assert_eq!(e.uuid, "7886c22d-835e-41a9-86fa-6f28c872ec3e");
        assert_eq!(e.session_id, "0d74b5ea-5186-4d10-8bda-7820a699573e");
        assert_eq!(e.cwd.as_deref(), Some("/home/mmn/github/nellie-rs"));
        assert_eq!(e.git_branch.as_deref(), Some("main"));
        assert_eq!(e.timestamp, 1_772_421_084);

        match &e.content {
            TranscriptContent::Human { text } => {
                assert!(text.contains("dev plan"));
            }
            other => panic!("expected Human, got {:?}", other),
        }
    }

    #[test]
    fn test_real_format_assistant_tool_use() {
        let jsonl = r#"{"parentUuid":"prev-uuid","isSidechain":false,"userType":"external","cwd":"/home/mmn/github/nellie-rs","sessionId":"sess-abc","version":"2.1.63","gitBranch":"main","slug":"peppy-foraging-bonbon","message":{"model":"claude-opus-4-6","id":"msg_abc","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01EQk8c5PRmvnKF1ogQ9EiMu","name":"Read","input":{"file_path":"/home/mmn/github/nellie-rs/DEVELOPMENT_PLAN.md","limit":100},"caller":{"type":"direct"}}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":10}},"requestId":"req_abc","type":"assistant","uuid":"d9fdc723-9f4f-46a6-80a4-e64f12fbcf19","timestamp":"2026-03-02T03:11:29.825Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);

        match &entries[0].content {
            TranscriptContent::ToolUse { name, input } => {
                assert_eq!(name, "Read");
                assert_eq!(
                    input["file_path"],
                    "/home/mmn/github/nellie-rs/DEVELOPMENT_PLAN.md"
                );
            }
            other => panic!("expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn test_real_format_tool_result() {
        let jsonl = r#"{"parentUuid":"prev","isSidechain":false,"userType":"external","cwd":"/tmp","sessionId":"sess","version":"2.1.63","gitBranch":"main","slug":"slug","sourceToolAssistantUUID":"src","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01EQk8c5PRmvnKF1ogQ9EiMu","content":"     1\t# README\n     2\t\n     3\tHello world"}]},"toolUseResult":{"type":"tool_result"},"type":"user","uuid":"result-uuid","timestamp":"2026-03-02T03:11:30.000Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);

        match &entries[0].content {
            TranscriptContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => {
                assert_eq!(tool_use_id, "toolu_01EQk8c5PRmvnKF1ogQ9EiMu");
                assert!(output.contains("README"));
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    // -- parse_transcript (file-based) --

    #[test]
    fn test_parse_transcript_file() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("test.jsonl");
        let content = r#"{"type":"user","message":{"role":"user","content":"hi"},"uuid":"u1","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;
        std::fs::write(&path, content).expect("write file");

        let entries = parse_transcript(&path).expect("parse should succeed");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_transcript_nonexistent_file() {
        let result = parse_transcript(Path::new("/nonexistent/file.jsonl"));
        assert!(result.is_err());
    }

    // -- Assistant with multiple tool calls --

    #[test]
    fn test_assistant_multiple_tool_calls() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"path":"a"}},{"type":"tool_use","id":"t2","name":"Read","input":{"path":"b"}}]},"uuid":"uuid-mt","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        // Multiple tool calls without text => Assistant variant
        // (not ToolUse, since there are multiple).
        match &entries[0].content {
            TranscriptContent::Assistant { text, tool_calls } => {
                assert!(text.is_empty());
                assert_eq!(tool_calls.len(), 2);
                assert_eq!(tool_calls[0].name, "Read");
                assert_eq!(tool_calls[1].name, "Read");
            }
            other => panic!("expected Assistant with tool_calls, got {:?}", other),
        }
    }

    // -- Assistant with text and tool calls mixed --

    #[test]
    fn test_assistant_text_and_tools_mixed() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I will read."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo hi"}}]},"uuid":"uuid-mix","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Assistant { text, tool_calls } => {
                assert_eq!(text, "I will read.");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].name, "Bash");
            }
            other => panic!("expected Assistant, got {:?}", other),
        }
    }

    // -- Tool result with missing is_error defaults to false --

    #[test]
    fn test_tool_result_missing_is_error() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t-x","content":"ok"}]},"uuid":"uuid-noe","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::ToolResult { is_error, .. } => {
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    // -- Empty content blocks --

    #[test]
    fn test_assistant_empty_content_blocks() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[]},"uuid":"uuid-empty","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Assistant { text, tool_calls } => {
                assert!(text.is_empty());
                assert!(tool_calls.is_empty());
            }
            other => panic!("expected Assistant, got {:?}", other),
        }
    }

    // -- Unknown content block types are silently skipped --

    #[test]
    fn test_unknown_content_block_type() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"server_tool_use","id":"st1","name":"mcp_tool"},{"type":"text","text":"Result follows."}]},"uuid":"uuid-unk","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}"#;

        let entries = parse_transcript_str(jsonl);
        assert_eq!(entries.len(), 1);
        match &entries[0].content {
            TranscriptContent::Assistant { text, tool_calls } => {
                assert_eq!(text, "Result follows.");
                assert!(tool_calls.is_empty());
            }
            other => panic!("expected Assistant, got {:?}", other),
        }
    }
}
