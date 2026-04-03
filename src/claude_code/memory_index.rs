//! MEMORY.md index manager for Claude Code's memory system.
//!
//! Claude Code reads `MEMORY.md` from
//! `~/.claude/projects/<project>/memory/MEMORY.md` as the master
//! index of available memory files. Each entry is a single Markdown
//! list item linking to a memory file with a short description hook.
//!
//! This module provides:
//!
//! - [`MemoryIndex`]: In-memory representation of a MEMORY.md file
//! - [`MemoryEntry`]: A single entry in the index
//! - Loading, saving, adding, removing, and querying entries
//! - Preservation of non-Nellie entries (manually added by users)
//! - 200-line limit enforcement (Claude Code truncates beyond that)
//! - `[nellie]` tagging to distinguish Nellie-managed entries
//!
//! # Entry Format
//!
//! Each Nellie-managed entry follows this format:
//!
//! ```text
//! - [Title](filename.md) -- one-line hook [nellie]
//! ```
//!
//! Non-Nellie entries (any line not ending with `[nellie]`) are
//! preserved verbatim during load/save cycles.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Error;

/// Maximum number of lines allowed in MEMORY.md.
///
/// Claude Code truncates the file beyond this limit, so we enforce
/// it proactively by trimming the oldest Nellie entries when the
/// file exceeds this threshold.
pub const MAX_MEMORY_LINES: usize = 200;

/// Maximum character length for a single MEMORY.md entry line.
///
/// Claude Code expects concise entries; this limit ensures each
/// entry stays readable in the index.
pub const MAX_ENTRY_LENGTH: usize = 150;

/// The tag appended to Nellie-managed entries for identification.
const NELLIE_TAG: &str = "[nellie]";

/// A single entry in the MEMORY.md index.
///
/// Entries come in two flavors:
/// - **Nellie-managed**: have a title, filename, and hook, tagged
///   with `[nellie]`.
/// - **Other**: lines not managed by Nellie, preserved verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryEntry {
    /// A Nellie-managed memory entry with structured fields.
    Nellie {
        /// Display title (appears in the link text).
        title: String,
        /// Filename of the linked memory file (e.g., `my_lesson.md`).
        filename: String,
        /// One-line description hook.
        hook: String,
    },
    /// A non-Nellie line preserved verbatim.
    ///
    /// This includes header lines, blank lines, manually-added
    /// entries, and any other content not managed by Nellie.
    Other(String),
}

impl MemoryEntry {
    /// Creates a new Nellie-managed entry.
    ///
    /// The hook will be truncated if the resulting line would exceed
    /// [`MAX_ENTRY_LENGTH`] characters.
    pub fn nellie(
        title: impl Into<String>,
        filename: impl Into<String>,
        hook: impl Into<String>,
    ) -> Self {
        Self::Nellie {
            title: title.into(),
            filename: filename.into(),
            hook: hook.into(),
        }
    }

    /// Returns `true` if this is a Nellie-managed entry.
    pub fn is_nellie(&self) -> bool {
        matches!(self, Self::Nellie { .. })
    }

    /// Returns the filename if this is a Nellie-managed entry.
    pub fn filename(&self) -> Option<&str> {
        match self {
            Self::Nellie { filename, .. } => Some(filename),
            Self::Other(_) => None,
        }
    }

    /// Formats this entry as a MEMORY.md line.
    ///
    /// Nellie entries are formatted as:
    /// ```text
    /// - [Title](filename.md) -- hook [nellie]
    /// ```
    ///
    /// When truncating to fit MAX_ENTRY_LENGTH, preserves the [nellie] tag
    /// by shortening the hook text instead.
    ///
    /// Other entries are returned as-is.
    ///
    /// # Truncation Strategy
    ///
    /// When the full line would exceed , the filename
    /// is **always preserved intact** (required for parsing on re-load).
    /// Truncation applies only to the hook, then the title, in order:
    ///
    /// 1. If hook fits with full title → truncate hook only.
    /// 2. If even an empty hook leaves no room for the title → truncate
    ///    title too, keeping as many chars as possible (min 4: "T...").
    /// 3. The  tag is always kept at the end.
    pub fn to_line(&self) -> String {
        match self {
            Self::Nellie {
                title,
                filename,
                hook,
            } => {
                let line = format!("- [{title}]({filename}) -- {hook} {NELLIE_TAG}");

                if line.len() <= MAX_ENTRY_LENGTH {
                    return line;
                }

                // Structural fixed overhead (filename is kept intact):
                //   "- [](filename) -- " + " [nellie]"
                // = 5 + filename.len() + 4 + " [nellie]".len()
                let tag_suffix = format!(" {NELLIE_TAG}"); // " [nellie]"
                                                           // Minimum structure without title or hook:
                                                           // "- [](filename) -- " = 3 + filename.len() + 6
                                                           // Format: "- [title](filename) -- hook [nellie]"
                                                           // Fixed parts: "- ["(3) + "]("(2) + ") -- "(5) + tag_suffix(9) = 19
                let base_len = 3 + 2 + filename.len() + 5 + tag_suffix.len();

                if base_len >= MAX_ENTRY_LENGTH {
                    // Even an empty title/hook exceeds budget.
                    // Fall back to a minimal parseable line.
                    return format!("- [...]({filename}) -- ... {NELLIE_TAG}");
                }

                // Budget available for (title + " -- " + hook) within MAX_ENTRY_LENGTH
                // Structure: "- [title](filename) -- hook [nellie]"
                // Fixed part without title and hook: "- [](filename) --  [nellie]" = base_len
                // Minimum title length: 4 chars ("T...")
                let min_title: usize = 4;
                let mut budget = MAX_ENTRY_LENGTH - base_len;

                // Try full title first, then truncate hook
                let title_len = title.len();
                if title_len <= budget {
                    // Title fits, truncate hook
                    let hook_budget = budget - title_len;
                    let truncated_hook = if hook.len() <= hook_budget {
                        hook.clone()
                    } else if hook_budget >= 3 {
                        format!("{}...", &hook[..hook_budget - 3])
                    } else {
                        String::new()
                    };
                    return format!("- [{title}]({filename}) -- {truncated_hook} {NELLIE_TAG}");
                }

                // Title doesn't fit either; truncate title (keep MIN_TITLE chars + "...")
                let title_budget = if budget > min_title {
                    budget.saturating_sub(3)
                } else {
                    budget.saturating_sub(1)
                };
                let short_title = if title_budget >= 3 {
                    format!("{}...", &title[..title_budget.min(title.len())])
                } else {
                    title[..min_title.min(title.len())].to_string()
                };
                budget = budget.saturating_sub(short_title.len());
                let truncated_hook = if hook.len() <= budget {
                    hook.clone()
                } else if budget >= 3 {
                    format!("{}...", &hook[..budget - 3])
                } else {
                    String::new()
                };
                format!("- [{short_title}]({filename}) -- {truncated_hook} {NELLIE_TAG}")
            }
            Self::Other(line) => line.clone(),
        }
    }
}

/// In-memory representation of a MEMORY.md index file.
///
/// The index maintains an ordered list of entries, preserving both
/// Nellie-managed entries (tagged with `[nellie]`) and non-Nellie
/// entries (headers, blank lines, manually-added entries).
///
/// # Example
///
/// ```rust,ignore
/// use nellie::claude_code::memory_index::MemoryIndex;
///
/// let mut index = MemoryIndex::new();
/// index.add_entry("My Lesson", "my_lesson.md", "SQLite WAL mode tips");
/// index.save(memory_dir.join("MEMORY.md"))?;
/// ```
#[derive(Debug, Clone)]
pub struct MemoryIndex {
    /// All entries in order (both Nellie and non-Nellie).
    entries: Vec<MemoryEntry>,
}

impl MemoryIndex {
    /// Creates an empty `MemoryIndex`.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Loads and parses a MEMORY.md file from disk.
    ///
    /// If the file does not exist, returns an empty index. This is
    /// not an error because the file may not have been created yet.
    ///
    /// Each line is parsed to determine if it is a Nellie-managed
    /// entry (ends with `[nellie]` and matches the link format) or
    /// a non-Nellie line (preserved verbatim).
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read.
    pub fn load(path: &Path) -> crate::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let content = fs::read_to_string(path).map_err(|e| {
            Error::Internal(format!(
                "failed to read MEMORY.md at '{}': {e}",
                path.display()
            ))
        })?;

        Ok(Self::parse(&content))
    }

    /// Parses the content of a MEMORY.md file into a `MemoryIndex`.
    fn parse(content: &str) -> Self {
        let mut entries = Vec::new();

        for line in content.lines() {
            if let Some(entry) = parse_nellie_entry(line) {
                entries.push(entry);
            } else {
                entries.push(MemoryEntry::Other(line.to_string()));
            }
        }

        Self { entries }
    }

    /// Adds a Nellie-managed entry to the index.
    ///
    /// If an entry with the same filename already exists, it is
    /// replaced (updated in place). Otherwise the new entry is
    /// appended at the end.
    ///
    /// # Arguments
    ///
    /// * `title` - Display title for the link.
    /// * `filename` - Filename of the memory file (e.g., `my_lesson.md`).
    /// * `hook` - One-line description hook.
    pub fn add_entry(
        &mut self,
        title: impl Into<String>,
        filename: impl Into<String>,
        hook: impl Into<String>,
    ) {
        let filename = filename.into();
        let entry = MemoryEntry::nellie(title, &filename, hook);

        // Replace existing entry with same filename
        if let Some(pos) = self.find_nellie_position(&filename) {
            self.entries[pos] = entry;
        } else {
            self.entries.push(entry);
        }
    }

    /// Removes a Nellie-managed entry by filename.
    ///
    /// Returns `true` if an entry was found and removed, `false`
    /// otherwise. Non-Nellie entries are never removed by this
    /// method.
    pub fn remove_entry(&mut self, filename: &str) -> bool {
        if let Some(pos) = self.find_nellie_position(filename) {
            self.entries.remove(pos);
            true
        } else {
            false
        }
    }

    /// Returns `true` if a Nellie-managed entry with the given
    /// filename exists in the index.
    pub fn has_entry(&mut self, filename: &str) -> bool {
        self.find_nellie_position(filename).is_some()
    }

    /// Returns a reference to all entries in the index.
    pub fn entries(&self) -> &[MemoryEntry] {
        &self.entries
    }

    /// Returns the total number of lines the index will produce
    /// when saved (including all entries).
    pub fn line_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns the number of Nellie-managed entries.
    pub fn nellie_entry_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_nellie()).count()
    }

    /// Enforces the maximum line limit by removing the oldest
    /// Nellie-managed entries.
    ///
    /// Non-Nellie entries are never removed. If the index is over
    /// the limit even after removing all Nellie entries, the
    /// remaining non-Nellie content is left as-is (we never delete
    /// user content).
    ///
    /// Entries are removed from the front (oldest first) since new
    /// entries are appended at the end.
    pub fn enforce_line_limit(&mut self, max_lines: usize) {
        while self.line_count() > max_lines {
            // Find the first Nellie entry to remove (oldest)
            let pos = self.entries.iter().position(MemoryEntry::is_nellie);

            match pos {
                Some(idx) => {
                    self.entries.remove(idx);
                }
                None => {
                    // No more Nellie entries to remove; we cannot
                    // trim non-Nellie content.
                    break;
                }
            }
        }
    }

    /// Saves the index to a MEMORY.md file using atomic writes.
    ///
    /// The line limit is enforced before writing. The file is
    /// written to a temporary path first, then renamed for
    /// atomicity.
    ///
    /// The parent directory is created if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation, file writing, or the
    /// rename fails.
    pub fn save(&mut self, path: &Path) -> crate::Result<PathBuf> {
        // Enforce line limit before saving
        self.enforce_line_limit(MAX_MEMORY_LINES);

        let content = self.render();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Atomic write: write to .tmp, then rename
        let tmp_path = path.with_extension("md.tmp");

        fs::write(&tmp_path, &content).map_err(|e| {
            Error::Internal(format!(
                "failed to write MEMORY.md temp file at '{}': {e}",
                tmp_path.display()
            ))
        })?;

        if let Err(e) = fs::rename(&tmp_path, path) {
            // Clean up temp file on failure
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Io(e));
        }

        Ok(path.to_path_buf())
    }

    /// Renders the index to a string suitable for writing to disk.
    fn render(&self) -> String {
        let lines: Vec<String> = self.entries.iter().map(MemoryEntry::to_line).collect();
        // Join with newlines and ensure a trailing newline
        let mut result = lines.join("\n");
        if !result.is_empty() {
            result.push('\n');
        }
        result
    }

    /// Finds the position of a Nellie entry by filename.
    fn find_nellie_position(&self, filename: &str) -> Option<usize> {
        self.entries.iter().position(|e| match e {
            MemoryEntry::Nellie { filename: f, .. } => f == filename,
            MemoryEntry::Other(_) => false,
        })
    }
}

impl Default for MemoryIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Attempts to parse a line as a Nellie-managed entry.
///
/// A Nellie entry matches the format:
/// ```text
/// - [Title](filename.md) -- hook [nellie]
/// ```
///
/// Returns `None` if the line does not match this format.
fn parse_nellie_entry(line: &str) -> Option<MemoryEntry> {
    let trimmed = line.trim();

    // Must end with the Nellie tag
    if !trimmed.ends_with(NELLIE_TAG) {
        return None;
    }

    // Must start with "- ["
    if !trimmed.starts_with("- [") {
        return None;
    }

    // Extract title: between "- [" and "]("
    let after_bracket = &trimmed[3..];
    let title_end = after_bracket.find("](")?;
    let title = after_bracket[..title_end].to_string();

    // Extract filename: between "](" and ")"
    let after_title = &after_bracket[title_end + 2..];
    let filename_end = after_title.find(')')?;
    let filename = after_title[..filename_end].to_string();

    // Extract hook: between " -- " and " [nellie]"
    let after_filename = &after_title[filename_end + 1..];
    let hook_part = after_filename
        .strip_prefix(" -- ")
        .or_else(|| after_filename.strip_prefix(" - "))?;
    let hook = hook_part
        .strip_suffix(&format!(" {NELLIE_TAG}"))
        .unwrap_or(hook_part)
        .trim()
        .to_string();

    if title.is_empty() || filename.is_empty() {
        return None;
    }

    Some(MemoryEntry::Nellie {
        title,
        filename,
        hook,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- MemoryEntry tests ---

    #[test]
    fn test_nellie_entry_to_line() {
        let entry = MemoryEntry::nellie("SQLite Tips", "sqlite_tips.md", "WAL mode best practices");
        assert_eq!(
            entry.to_line(),
            "- [SQLite Tips](sqlite_tips.md) -- WAL mode best practices [nellie]"
        );
    }

    #[test]
    fn test_other_entry_to_line() {
        let entry = MemoryEntry::Other("# My Project Memory".to_string());
        assert_eq!(entry.to_line(), "# My Project Memory");
    }

    #[test]
    fn test_entry_is_nellie() {
        let nellie = MemoryEntry::nellie("Title", "file.md", "hook");
        let other = MemoryEntry::Other("some line".to_string());
        assert!(nellie.is_nellie());
        assert!(!other.is_nellie());
    }

    #[test]
    fn test_entry_filename() {
        let nellie = MemoryEntry::nellie("Title", "file.md", "hook");
        let other = MemoryEntry::Other("some line".to_string());
        assert_eq!(nellie.filename(), Some("file.md"));
        assert_eq!(other.filename(), None);
    }

    #[test]
    fn test_entry_line_truncation() {
        let long_hook = "a".repeat(200);
        let entry = MemoryEntry::nellie("Title", "file.md", &long_hook);
        let line = entry.to_line();
        assert!(
            line.len() <= MAX_ENTRY_LENGTH,
            "line length {} exceeds max {}",
            line.len(),
            MAX_ENTRY_LENGTH
        );
        // Verify [nellie] tag is preserved after truncation
        assert!(
            line.ends_with(NELLIE_TAG),
            "truncated line should preserve [nellie] tag: {line}"
        );
    }

    #[test]
    fn test_entry_truncation_preserves_nellie_tag() {
        // Create an entry with a very long hook that forces truncation
        let long_hook = "This is a very long description about something important ".repeat(5);
        let entry = MemoryEntry::nellie("SQLite Tips", "sqlite_tips.md", &long_hook);
        let line = entry.to_line();

        // Verify it's truncated
        assert!(
            line.len() <= MAX_ENTRY_LENGTH,
            "line should be truncated: {} > {}",
            line.len(),
            MAX_ENTRY_LENGTH
        );

        // Verify [nellie] tag is at the end
        assert!(
            line.ends_with(&format!(" {NELLIE_TAG}")),
            "line should end with ' [nellie]': {line}"
        );

        // Verify the line can still be parsed back
        let parsed = parse_nellie_entry(&line);
        assert!(
            parsed.is_some(),
            "truncated line should still parse as valid entry"
        );
    }

    // --- parse_nellie_entry tests ---

    #[test]
    fn test_parse_valid_nellie_entry() {
        let line = "- [My Lesson](my_lesson.md) -- Important stuff [nellie]";
        let entry = parse_nellie_entry(line).unwrap();
        match entry {
            MemoryEntry::Nellie {
                title,
                filename,
                hook,
            } => {
                assert_eq!(title, "My Lesson");
                assert_eq!(filename, "my_lesson.md");
                assert_eq!(hook, "Important stuff");
            }
            MemoryEntry::Other(_) => panic!("expected Nellie entry"),
        }
    }

    #[test]
    fn test_parse_non_nellie_entry() {
        let line = "- [Manual Entry](manual.md) -- Added by hand";
        assert!(parse_nellie_entry(line).is_none());
    }

    #[test]
    fn test_parse_header_line() {
        let line = "# Nellie-RS Memory Index";
        assert!(parse_nellie_entry(line).is_none());
    }

    #[test]
    fn test_parse_blank_line() {
        let line = "";
        assert!(parse_nellie_entry(line).is_none());
    }

    #[test]
    fn test_parse_entry_without_link() {
        let line = "- Some plain text [nellie]";
        assert!(parse_nellie_entry(line).is_none());
    }

    #[test]
    fn test_parse_roundtrip() {
        let entry = MemoryEntry::nellie("Git Rules", "git_rules.md", "Interleaving workflow");
        let line = entry.to_line();
        let parsed = parse_nellie_entry(&line).unwrap();
        assert_eq!(parsed, entry);
    }

    // --- MemoryIndex basic tests ---

    #[test]
    fn test_new_index_is_empty() {
        let index = MemoryIndex::new();
        assert_eq!(index.line_count(), 0);
        assert_eq!(index.nellie_entry_count(), 0);
    }

    #[test]
    fn test_add_entry() {
        let mut index = MemoryIndex::new();
        index.add_entry("Test", "test.md", "A test entry");
        assert_eq!(index.nellie_entry_count(), 1);
        assert!(index.has_entry("test.md"));
    }

    #[test]
    fn test_add_duplicate_replaces() {
        let mut index = MemoryIndex::new();
        index.add_entry("Test V1", "test.md", "First version");
        index.add_entry("Test V2", "test.md", "Second version");

        assert_eq!(index.nellie_entry_count(), 1);

        // Verify the updated entry
        let entry = index
            .entries()
            .iter()
            .find(|e| e.filename() == Some("test.md"))
            .unwrap();
        match entry {
            MemoryEntry::Nellie { title, hook, .. } => {
                assert_eq!(title, "Test V2");
                assert_eq!(hook, "Second version");
            }
            _ => panic!("expected Nellie entry"),
        }
    }

    #[test]
    fn test_remove_entry() {
        let mut index = MemoryIndex::new();
        index.add_entry("Remove Me", "remove.md", "Will be removed");

        assert!(index.remove_entry("remove.md"));
        assert!(!index.has_entry("remove.md"));
        assert_eq!(index.nellie_entry_count(), 0);
    }

    #[test]
    fn test_remove_nonexistent_entry() {
        let mut index = MemoryIndex::new();
        assert!(!index.remove_entry("nonexistent.md"));
    }

    #[test]
    fn test_has_entry() {
        let mut index = MemoryIndex::new();
        index.add_entry("Exists", "exists.md", "I exist");
        assert!(index.has_entry("exists.md"));
        assert!(!index.has_entry("missing.md"));
    }

    // --- Parsing tests ---

    #[test]
    fn test_parse_mixed_content() {
        let content = "\
# Nellie-RS Memory Index

- [project_deployment.md](project_deployment.md) -- Nellie-RS only runs on MiniDev, hostname guard enforces this
- [SQLite Tips](sqlite_tips.md) -- WAL mode best practices [nellie]
- [Git Rules](git_rules.md) -- Interleaving workflow [nellie]
";
        let index = MemoryIndex::parse(content);

        // 5 lines total (header, blank line, manual entry, 2 nellie
        // entries) -- note the trailing newline does not add a line
        // because lines() excludes it
        assert_eq!(index.line_count(), 5);
        assert_eq!(index.nellie_entry_count(), 2);
    }

    #[test]
    fn test_parse_preserves_header() {
        let content = "# My Custom Header\n\n- [Note](note.md) -- A note [nellie]\n";
        let index = MemoryIndex::parse(content);

        assert_eq!(
            index.entries[0],
            MemoryEntry::Other("# My Custom Header".to_string())
        );
        assert_eq!(index.entries[1], MemoryEntry::Other(String::new()));
        assert!(index.entries[2].is_nellie());
    }

    #[test]
    fn test_parse_preserves_manual_entries() {
        let content = "\
- [Manual](manual.md) -- Manually added entry
- [Auto](auto.md) -- Auto-managed [nellie]
";
        let index = MemoryIndex::parse(content);

        assert_eq!(index.line_count(), 2);
        assert_eq!(index.nellie_entry_count(), 1);
        assert!(!index.entries[0].is_nellie());
        assert!(index.entries[1].is_nellie());
    }

    // --- Line limit enforcement ---

    #[test]
    fn test_enforce_line_limit_removes_oldest_nellie() {
        let mut index = MemoryIndex::new();

        // Add a header and manual entry (2 non-Nellie lines)
        index
            .entries
            .push(MemoryEntry::Other("# Memory Index".to_string()));
        index.entries.push(MemoryEntry::Other(String::new()));

        // Add 5 Nellie entries
        for i in 0..5 {
            index.add_entry(
                format!("Entry {i}"),
                format!("entry_{i}.md"),
                format!("Hook {i}"),
            );
        }

        assert_eq!(index.line_count(), 7);

        // Enforce a limit of 5 lines
        index.enforce_line_limit(5);

        assert_eq!(index.line_count(), 5);
        // The 2 non-Nellie lines survive
        assert!(!index.entries[0].is_nellie());
        assert!(!index.entries[1].is_nellie());
        // Only 3 Nellie entries remain (oldest removed first)
        assert_eq!(index.nellie_entry_count(), 3);
        // The remaining entries should be 2, 3, 4 (0 and 1 removed)
        assert!(index.has_entry("entry_2.md"));
        assert!(index.has_entry("entry_3.md"));
        assert!(index.has_entry("entry_4.md"));
        assert!(!index.has_entry("entry_0.md"));
        assert!(!index.has_entry("entry_1.md"));
    }

    #[test]
    fn test_enforce_line_limit_never_removes_non_nellie() {
        let mut index = MemoryIndex::new();

        // Add 10 non-Nellie lines
        for i in 0..10 {
            index.entries.push(MemoryEntry::Other(format!("Line {i}")));
        }

        // Even with a limit of 5, non-Nellie lines are preserved
        index.enforce_line_limit(5);
        assert_eq!(index.line_count(), 10);
    }

    #[test]
    fn test_enforce_line_limit_under_limit_noop() {
        let mut index = MemoryIndex::new();
        index.add_entry("Only One", "one.md", "Single entry");

        index.enforce_line_limit(200);
        assert_eq!(index.nellie_entry_count(), 1);
    }

    #[test]
    fn test_enforce_200_line_limit() {
        let mut index = MemoryIndex::new();

        // Add a header
        index
            .entries
            .push(MemoryEntry::Other("# Memory".to_string()));

        // Add 250 Nellie entries (way over the 200-line limit)
        for i in 0..250 {
            index.add_entry(format!("E{i}"), format!("e{i}.md"), format!("Hook {i}"));
        }

        assert_eq!(index.line_count(), 251);

        index.enforce_line_limit(MAX_MEMORY_LINES);

        assert!(
            index.line_count() <= MAX_MEMORY_LINES,
            "line count {} exceeds max {}",
            index.line_count(),
            MAX_MEMORY_LINES,
        );
        // Header preserved
        assert_eq!(index.entries[0], MemoryEntry::Other("# Memory".to_string()));
        // 199 Nellie entries (200 - 1 header)
        assert_eq!(index.nellie_entry_count(), 199);
    }

    // --- Save/load roundtrip tests ---

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");

        let mut index = MemoryIndex::new();
        index
            .entries
            .push(MemoryEntry::Other("# Nellie-RS Memory Index".to_string()));
        index.entries.push(MemoryEntry::Other(String::new()));
        index.add_entry("SQLite Tips", "sqlite_tips.md", "WAL mode best practices");
        index.add_entry("Git Rules", "git_rules.md", "Interleaving workflow");

        index.save(&path).unwrap();
        assert!(path.exists());

        let loaded = MemoryIndex::load(&path).unwrap();

        assert_eq!(loaded.line_count(), index.line_count());
        assert_eq!(loaded.nellie_entry_count(), index.nellie_entry_count());
        assert!(loaded.entries[0] == MemoryEntry::Other("# Nellie-RS Memory Index".to_string()));
        assert!(loaded.entries[1] == MemoryEntry::Other(String::new()));
    }

    #[test]
    fn test_load_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.md");

        let index = MemoryIndex::load(&path).unwrap();
        assert_eq!(index.line_count(), 0);
    }

    #[test]
    fn test_save_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("sub").join("dir").join("MEMORY.md");

        let mut index = MemoryIndex::new();
        index.add_entry("Test", "test.md", "A test");

        index.save(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn test_save_no_leftover_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");

        let mut index = MemoryIndex::new();
        index.add_entry("Test", "test.md", "A test");
        index.save(&path).unwrap();

        // Verify no .tmp files remain
        for entry in fs::read_dir(dir.path()).unwrap().flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            assert!(!name_str.ends_with(".tmp"), "leftover tmp file: {name_str}");
        }
    }

    #[test]
    fn test_save_enforces_line_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");

        let mut index = MemoryIndex::new();
        for i in 0..250 {
            index.add_entry(format!("E{i}"), format!("e{i}.md"), format!("Hook {i}"));
        }

        index.save(&path).unwrap();

        // Read back and verify line count
        let content = fs::read_to_string(&path).unwrap();
        let line_count = content.lines().count();
        assert!(
            line_count <= MAX_MEMORY_LINES,
            "saved file has {line_count} lines, max is {MAX_MEMORY_LINES}"
        );
    }

    // --- Preservation tests ---

    #[test]
    fn test_preserve_non_nellie_entries_on_add() {
        let content = "\
# My Project Index

- [Manual Entry](manual.md) -- Hand-written note
- [Auto Entry](auto.md) -- Auto hook [nellie]
";
        let mut index = MemoryIndex::parse(content);

        // Add a new Nellie entry
        index.add_entry("New", "new.md", "Brand new");

        // Manual entry and header still present
        assert_eq!(index.line_count(), 5);
        assert_eq!(
            index.entries[0],
            MemoryEntry::Other("# My Project Index".to_string())
        );
        assert_eq!(
            index.entries[2],
            MemoryEntry::Other("- [Manual Entry](manual.md) -- Hand-written note".to_string())
        );
    }

    #[test]
    fn test_preserve_non_nellie_entries_on_remove() {
        let content = "\
# Index
- [Manual](manual.md) -- Manual note
- [Auto](auto.md) -- Auto hook [nellie]
";
        let mut index = MemoryIndex::parse(content);

        index.remove_entry("auto.md");

        assert_eq!(index.line_count(), 2);
        assert_eq!(index.entries[0], MemoryEntry::Other("# Index".to_string()));
        assert_eq!(
            index.entries[1],
            MemoryEntry::Other("- [Manual](manual.md) -- Manual note".to_string())
        );
    }

    #[test]
    fn test_full_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");

        // Start with an existing MEMORY.md that has manual content
        let initial = "\
# Nellie-RS Memory Index

- [project_deployment.md](project_deployment.md) -- Nellie-RS only runs on MiniDev
";
        fs::write(&path, initial).unwrap();

        // Load it
        let mut index = MemoryIndex::load(&path).unwrap();
        assert_eq!(index.line_count(), 3);
        assert_eq!(index.nellie_entry_count(), 0); // No [nellie] tags

        // Add Nellie entries
        index.add_entry(
            "SQLite WAL",
            "sqlite_wal.md",
            "Use WAL mode for concurrent reads",
        );
        index.add_entry(
            "Git Workflow",
            "git_workflow.md",
            "One branch per task, squash merge",
        );

        // Save
        index.save(&path).unwrap();

        // Reload and verify
        let reloaded = MemoryIndex::load(&path).unwrap();
        assert_eq!(reloaded.line_count(), 5);
        assert_eq!(reloaded.nellie_entry_count(), 2);

        // Original content preserved
        assert_eq!(
            reloaded.entries[0],
            MemoryEntry::Other("# Nellie-RS Memory Index".to_string())
        );
    }

    // --- Edge cases ---

    #[test]
    fn test_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        fs::write(&path, "").unwrap();

        let index = MemoryIndex::load(&path).unwrap();
        assert_eq!(index.line_count(), 0);
    }

    #[test]
    fn test_entry_with_special_chars_in_title() {
        let entry = MemoryEntry::nellie(
            "SQLite: WAL Mode (v2)",
            "sqlite_wal_mode_v2.md",
            "Configuration tips",
        );
        let line = entry.to_line();
        let parsed = parse_nellie_entry(&line).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn test_multiple_adds_and_removes() {
        let mut index = MemoryIndex::new();

        // Add 10 entries
        for i in 0..10 {
            index.add_entry(
                format!("Entry {i}"),
                format!("entry_{i}.md"),
                format!("Hook {i}"),
            );
        }
        assert_eq!(index.nellie_entry_count(), 10);

        // Remove odd entries
        for i in (1..10).step_by(2) {
            assert!(index.remove_entry(&format!("entry_{i}.md")));
        }
        assert_eq!(index.nellie_entry_count(), 5);

        // Even entries still present
        for i in (0..10).step_by(2) {
            assert!(index.has_entry(&format!("entry_{i}.md")));
        }
    }

    #[test]
    fn test_default_impl() {
        let index = MemoryIndex::default();
        assert_eq!(index.line_count(), 0);
    }

    #[test]
    fn test_save_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");

        // Write initial content
        let mut index1 = MemoryIndex::new();
        index1.add_entry("First", "first.md", "Initial");
        index1.save(&path).unwrap();

        // Overwrite with different content
        let mut index2 = MemoryIndex::new();
        index2.add_entry("Second", "second.md", "Replacement");
        index2.save(&path).unwrap();

        // Verify only second content exists
        let loaded = MemoryIndex::load(&path).unwrap();
        assert_eq!(loaded.nellie_entry_count(), 1);
        assert!(!loaded
            .entries
            .iter()
            .any(|e| e.filename() == Some("first.md")));
    }

    #[test]
    fn test_line_limit_with_mixed_entries() {
        let mut index = MemoryIndex::new();

        // 3 non-Nellie lines (header, blank, manual entry)
        index
            .entries
            .push(MemoryEntry::Other("# Index".to_string()));
        index.entries.push(MemoryEntry::Other(String::new()));
        index.entries.push(MemoryEntry::Other(
            "- [Manual](manual.md) -- Keep me".to_string(),
        ));

        // 10 Nellie entries
        for i in 0..10 {
            index.add_entry(format!("N{i}"), format!("n{i}.md"), format!("Hook {i}"));
        }

        // Total: 13 lines. Enforce limit of 8.
        index.enforce_line_limit(8);

        // Should have 3 non-Nellie + 5 Nellie = 8
        assert_eq!(index.line_count(), 8);
        assert_eq!(index.nellie_entry_count(), 5);

        // Oldest Nellie entries (0-4) removed, 5-9 remain
        assert!(!index.has_entry("n0.md"));
        assert!(!index.has_entry("n4.md"));
        assert!(index.has_entry("n5.md"));
        assert!(index.has_entry("n9.md"));
    }

    #[test]
    fn test_entry_line_truncation_long_title() {
        // Regression test: title+filename alone can exceed MAX_ENTRY_LENGTH.
        // usize subtraction must not overflow — saturating_sub must be used.
        let title = "Entra ID Connect: Alternate UPN suffix when AD uses non-routable domain";
        let filename = "entra_id_connect_alternate_upn_suffix_when_ad_uses_non-routable_domain.md";
        let hook = "[critical] When on-prem AD only has a non-routable UPN suffix";
        let entry = MemoryEntry::nellie(title, filename, hook);
        let line = entry.to_line();
        assert!(
            line.len() <= MAX_ENTRY_LENGTH,
            "long-title entry length {} exceeds max {}: {}",
            line.len(),
            MAX_ENTRY_LENGTH,
            line
        );
        assert!(
            line.ends_with(NELLIE_TAG),
            "truncated long-title line must keep [nellie] tag: {line}"
        );
    }
}
