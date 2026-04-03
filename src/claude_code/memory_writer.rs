//! Memory file writer for Claude Code's native memory system.
//!
//! Claude Code loads memory files from
//! `~/.claude/projects/<project>/memory/` at session start. Each
//! memory file is a Markdown document with YAML frontmatter
//! specifying the name, description, and type.
//!
//! This module provides:
//!
//! - [`MemoryType`]: The four memory types Claude Code recognizes
//! - [`MemoryFile`]: In-memory representation of a memory file
//! - [`write_memory_file`]: Atomic write (`.tmp` + rename) to
//!   prevent partial file corruption
//! - [`read_memory_file`]: Parse an existing memory file from disk
//! - [`delete_memory_file`]: Remove a memory file
//! - [`memory_filename`]: Derive a safe filename from a title

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// The four memory types that Claude Code recognizes in frontmatter.
///
/// These map to the `type` field in the YAML frontmatter of memory
/// files. Claude Code uses these to categorize and prioritize
/// memories during context loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// User-defined memories (preferences, conventions).
    User,
    /// Feedback from corrections or mistakes.
    Feedback,
    /// Project-specific knowledge (architecture, patterns).
    Project,
    /// Reference material (API docs, specs).
    Reference,
}

impl fmt::Display for MemoryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Feedback => write!(f, "feedback"),
            Self::Project => write!(f, "project"),
            Self::Reference => write!(f, "reference"),
        }
    }
}

impl std::str::FromStr for MemoryType {
    type Err = Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        match s.to_lowercase().as_str() {
            "user" => Ok(Self::User),
            "feedback" => Ok(Self::Feedback),
            "project" => Ok(Self::Project),
            "reference" => Ok(Self::Reference),
            other => Err(Error::Internal(format!("unknown memory type: '{other}'"))),
        }
    }
}

/// In-memory representation of a Claude Code memory file.
///
/// A memory file consists of YAML frontmatter (name, description,
/// type) followed by Markdown content. When serialized to disk the
/// format is:
///
/// ```text
/// ---
/// name: My Memory
/// description: A brief description
/// type: project
/// ---
///
/// The actual content goes here.
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFile {
    /// Display name for the memory.
    pub name: String,
    /// One-line description (used in MEMORY.md index entries).
    pub description: String,
    /// Classification for Claude Code's context loading.
    pub memory_type: MemoryType,
    /// The Markdown body content.
    pub content: String,
}

impl MemoryFile {
    /// Creates a new `MemoryFile`.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        memory_type: MemoryType,
        content: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            memory_type,
            content: content.into(),
        }
    }

    /// Returns the canonical filename for this memory file.
    ///
    /// The filename is derived from the `name` field: lowercased,
    /// spaces replaced with underscores, non-alphanumeric characters
    /// (except underscores and hyphens) removed, with `.md` appended.
    pub fn filename(&self) -> String {
        memory_filename(&self.name)
    }

    /// Serializes this memory file to the Claude Code frontmatter
    /// format.
    ///
    /// The output is a complete Markdown document with YAML
    /// frontmatter.
    pub fn to_markdown(&self) -> String {
        format!(
            "---\n\
             name: {}\n\
             description: {}\n\
             type: {}\n\
             ---\n\
             \n\
             {}",
            self.name, self.description, self.memory_type, self.content
        )
    }
}

/// Derives a safe filename from a memory title.
///
/// The conversion:
/// 1. Convert to lowercase
/// 2. Replace spaces with underscores
/// 3. Remove characters that are not alphanumeric, underscore, or
///    hyphen
/// 4. Collapse consecutive underscores
/// 5. Trim leading/trailing underscores
/// 6. Append `.md`
///
/// If the title is empty or reduces to nothing after sanitization,
/// the filename `_unnamed.md` is returned.
///
/// # Examples
///
/// ```rust,ignore
/// # use nellie::claude_code::memory_writer::memory_filename;
/// assert_eq!(memory_filename("My Cool Lesson"), "my_cool_lesson.md");
/// assert_eq!(memory_filename("SQLite WAL Mode"), "sqlite_wal_mode.md");
/// ```
pub fn memory_filename(title: &str) -> String {
    let mut name: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '_' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();

    // Collapse consecutive underscores
    while name.contains("__") {
        name = name.replace("__", "_");
    }

    // Trim leading/trailing underscores
    let name = name.trim_matches('_');

    if name.is_empty() {
        "_unnamed.md".to_string()
    } else {
        format!("{name}.md")
    }
}

/// Writes a memory file atomically to the given directory.
///
/// The write is performed in two steps to prevent partial file
/// corruption:
/// 1. Write the full content to a temporary file (`.tmp` suffix)
///    in the same directory.
/// 2. Rename the temporary file to the final path (atomic on most
///    filesystems).
///
/// The directory is created if it does not exist.
///
/// # Arguments
///
/// * `dir` - The directory to write the memory file into.
/// * `file` - The memory file to write.
///
/// # Returns
///
/// The path to the written file.
///
/// # Errors
///
/// Returns an error if directory creation, file writing, or the
/// rename operation fails.
pub fn write_memory_file(dir: &Path, file: &MemoryFile) -> crate::Result<PathBuf> {
    // Ensure the directory exists
    fs::create_dir_all(dir)?;

    let filename = file.filename();
    let final_path = dir.join(&filename);
    let tmp_path = dir.join(format!(".{filename}.tmp"));

    let content = file.to_markdown();

    // Step 1: Write to temporary file
    fs::write(&tmp_path, &content)?;

    // Step 2: Atomic rename
    if let Err(e) = fs::rename(&tmp_path, &final_path) {
        // Clean up the temp file on rename failure
        let _ = fs::remove_file(&tmp_path);
        return Err(Error::Io(e));
    }

    Ok(final_path)
}

/// Reads and parses a memory file from disk.
///
/// The file must have YAML frontmatter delimited by `---` lines,
/// containing `name`, `description`, and `type` fields.
///
/// # Arguments
///
/// * `path` - Path to the `.md` memory file.
///
/// # Errors
///
/// Returns an error if the file cannot be read, or if the
/// frontmatter is missing or malformed.
pub fn read_memory_file(path: &Path) -> crate::Result<MemoryFile> {
    let raw = fs::read_to_string(path).map_err(|e| {
        Error::Internal(format!(
            "failed to read memory file '{}': {e}",
            path.display()
        ))
    })?;

    parse_memory_file(&raw, path)
}

/// Deletes a memory file from disk.
///
/// This is a simple wrapper around [`std::fs::remove_file`] that
/// maps the error to Nellie's error type.
///
/// # Errors
///
/// Returns an error if the file does not exist or cannot be removed.
pub fn delete_memory_file(path: &Path) -> crate::Result<()> {
    fs::remove_file(path).map_err(|e| {
        Error::Internal(format!(
            "failed to delete memory file '{}': {e}",
            path.display()
        ))
    })
}

/// Parses raw Markdown with YAML frontmatter into a `MemoryFile`.
///
/// Expected format:
/// ```text
/// ---
/// name: ...
/// description: ...
/// type: ...
/// ---
///
/// content here
/// ```
fn parse_memory_file(raw: &str, path: &Path) -> crate::Result<MemoryFile> {
    let trimmed = raw.trim_start();

    // Must start with frontmatter delimiter
    if !trimmed.starts_with("---") {
        return Err(Error::Internal(format!(
            "memory file '{}' has no YAML frontmatter",
            path.display()
        )));
    }

    // Find the closing delimiter
    let after_open = &trimmed[3..].trim_start_matches(['\r', '\n']);
    let close_pos = after_open.find("\n---").ok_or_else(|| {
        Error::Internal(format!(
            "memory file '{}' has unclosed YAML frontmatter",
            path.display()
        ))
    })?;

    let frontmatter = &after_open[..close_pos];
    // Content starts after the closing "---" line
    let rest = &after_open[close_pos + 4..]; // skip "\n---"
    let content = rest.trim_start_matches(['\r', '\n']).to_string();

    // Parse frontmatter fields
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut memory_type: Option<MemoryType> = None;

    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => name = Some(value.to_string()),
                "description" => description = Some(value.to_string()),
                "type" => memory_type = Some(value.parse()?),
                _ => {} // Ignore unknown keys
            }
        }
    }

    let name = name.ok_or_else(|| {
        Error::Internal(format!(
            "memory file '{}': missing 'name' in frontmatter",
            path.display()
        ))
    })?;
    let description = description.ok_or_else(|| {
        Error::Internal(format!(
            "memory file '{}': missing 'description' in frontmatter",
            path.display()
        ))
    })?;
    let memory_type = memory_type.ok_or_else(|| {
        Error::Internal(format!(
            "memory file '{}': missing 'type' in frontmatter",
            path.display()
        ))
    })?;

    Ok(MemoryFile {
        name,
        description,
        memory_type,
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- MemoryType tests ---

    #[test]
    fn test_memory_type_display() {
        assert_eq!(MemoryType::User.to_string(), "user");
        assert_eq!(MemoryType::Feedback.to_string(), "feedback");
        assert_eq!(MemoryType::Project.to_string(), "project");
        assert_eq!(MemoryType::Reference.to_string(), "reference");
    }

    #[test]
    fn test_memory_type_from_str() {
        assert_eq!("user".parse::<MemoryType>().unwrap(), MemoryType::User);
        assert_eq!(
            "feedback".parse::<MemoryType>().unwrap(),
            MemoryType::Feedback
        );
        assert_eq!(
            "project".parse::<MemoryType>().unwrap(),
            MemoryType::Project
        );
        assert_eq!(
            "reference".parse::<MemoryType>().unwrap(),
            MemoryType::Reference
        );
    }

    #[test]
    fn test_memory_type_from_str_case_insensitive() {
        assert_eq!("User".parse::<MemoryType>().unwrap(), MemoryType::User);
        assert_eq!(
            "FEEDBACK".parse::<MemoryType>().unwrap(),
            MemoryType::Feedback
        );
        assert_eq!(
            "Project".parse::<MemoryType>().unwrap(),
            MemoryType::Project
        );
    }

    #[test]
    fn test_memory_type_from_str_invalid() {
        assert!("unknown".parse::<MemoryType>().is_err());
        assert!("".parse::<MemoryType>().is_err());
    }

    #[test]
    fn test_memory_type_serde_roundtrip() {
        let types = [
            MemoryType::User,
            MemoryType::Feedback,
            MemoryType::Project,
            MemoryType::Reference,
        ];
        for t in &types {
            let json = serde_json::to_string(t).unwrap();
            let back: MemoryType = serde_json::from_str(&json).unwrap();
            assert_eq!(*t, back);
        }
    }

    // --- memory_filename tests ---

    #[test]
    fn test_filename_basic() {
        assert_eq!(memory_filename("My Cool Lesson"), "my_cool_lesson.md");
    }

    #[test]
    fn test_filename_special_characters() {
        assert_eq!(
            memory_filename("SQLite: WAL Mode (v2)"),
            "sqlite_wal_mode_v2.md"
        );
    }

    #[test]
    fn test_filename_hyphens_preserved() {
        assert_eq!(memory_filename("pre-commit hooks"), "pre-commit_hooks.md");
    }

    #[test]
    fn test_filename_multiple_spaces() {
        assert_eq!(memory_filename("too   many   spaces"), "too_many_spaces.md");
    }

    #[test]
    fn test_filename_leading_trailing_spaces() {
        assert_eq!(memory_filename("  padded  "), "padded.md");
    }

    #[test]
    fn test_filename_empty_string() {
        assert_eq!(memory_filename(""), "_unnamed.md");
    }

    #[test]
    fn test_filename_only_special_chars() {
        assert_eq!(memory_filename("!@#$%^&*()"), "_unnamed.md");
    }

    #[test]
    fn test_filename_unicode() {
        // Non-ASCII alphanumerics are kept by is_alphanumeric
        let result = memory_filename("cafe latte");
        assert_eq!(result, "cafe_latte.md");
    }

    // --- MemoryFile tests ---

    #[test]
    fn test_memory_file_new() {
        let mf = MemoryFile::new(
            "Test Memory",
            "A test memory",
            MemoryType::Project,
            "Some content here.",
        );
        assert_eq!(mf.name, "Test Memory");
        assert_eq!(mf.description, "A test memory");
        assert_eq!(mf.memory_type, MemoryType::Project);
        assert_eq!(mf.content, "Some content here.");
    }

    #[test]
    fn test_memory_file_filename() {
        let mf = MemoryFile::new(
            "Git Interleaving Rules",
            "How to branch",
            MemoryType::Reference,
            "content",
        );
        assert_eq!(mf.filename(), "git_interleaving_rules.md");
    }

    #[test]
    fn test_memory_file_to_markdown() {
        let mf = MemoryFile::new(
            "My Memory",
            "A brief description",
            MemoryType::Project,
            "The actual content goes here.",
        );
        let md = mf.to_markdown();
        let expected = "\
---
name: My Memory
description: A brief description
type: project
---

The actual content goes here.";
        assert_eq!(md, expected);
    }

    // --- parse_memory_file tests ---

    #[test]
    fn test_parse_roundtrip() {
        let original = MemoryFile::new(
            "Round Trip",
            "Testing serialization",
            MemoryType::Feedback,
            "Content survives the trip.",
        );
        let md = original.to_markdown();
        let parsed = parse_memory_file(&md, Path::new("test.md")).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_parse_no_frontmatter() {
        let raw = "Just some plain text.";
        let result = parse_memory_file(raw, Path::new("bad.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_unclosed_frontmatter() {
        let raw = "---\nname: Oops\ndescription: Missing close\n";
        let result = parse_memory_file(raw, Path::new("bad.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_missing_name() {
        let raw = "---\ndescription: hi\ntype: user\n---\ncontent";
        let result = parse_memory_file(raw, Path::new("bad.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_missing_description() {
        let raw = "---\nname: hi\ntype: user\n---\ncontent";
        let result = parse_memory_file(raw, Path::new("bad.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_missing_type() {
        let raw = "---\nname: hi\ndescription: bye\n---\ncontent";
        let result = parse_memory_file(raw, Path::new("bad.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_unknown_keys_ignored() {
        let raw = "\
---
name: Test
description: Desc
type: user
extra_key: should be ignored
---

Content here.";
        let parsed = parse_memory_file(raw, Path::new("test.md")).unwrap();
        assert_eq!(parsed.name, "Test");
        assert_eq!(parsed.content, "Content here.");
    }

    #[test]
    fn test_parse_multiline_content() {
        let raw = "\
---
name: Multi
description: Multiline test
type: reference
---

Line one.

Line two.

Line three.";
        let parsed = parse_memory_file(raw, Path::new("test.md")).unwrap();
        assert_eq!(parsed.content, "Line one.\n\nLine two.\n\nLine three.");
    }

    // --- File I/O tests (using tempdir) ---

    #[test]
    fn test_write_and_read_memory_file() {
        let dir = tempfile::tempdir().unwrap();
        let mf = MemoryFile::new(
            "Write Test",
            "Testing write then read",
            MemoryType::Project,
            "Hello from the test.",
        );

        let path = write_memory_file(dir.path(), &mf).unwrap();

        assert!(path.exists(), "file should exist after write");
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "write_test.md");

        let read_back = read_memory_file(&path).unwrap();
        assert_eq!(read_back, mf);
    }

    #[test]
    fn test_write_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("sub").join("dir");

        let mf = MemoryFile::new("Nested", "In a nested dir", MemoryType::User, "content");

        let path = write_memory_file(&nested, &mf).unwrap();
        assert!(path.exists());
        assert!(nested.exists());
    }

    #[test]
    fn test_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();

        let mf_v1 = MemoryFile::new(
            "Overwrite",
            "Version 1",
            MemoryType::Project,
            "First version.",
        );
        let mf_v2 = MemoryFile::new(
            "Overwrite",
            "Version 2",
            MemoryType::Feedback,
            "Second version.",
        );

        write_memory_file(dir.path(), &mf_v1).unwrap();
        let path = write_memory_file(dir.path(), &mf_v2).unwrap();

        let read_back = read_memory_file(&path).unwrap();
        assert_eq!(read_back, mf_v2);
    }

    #[test]
    fn test_write_no_leftover_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let mf = MemoryFile::new("Clean", "No tmp leftovers", MemoryType::User, "content");

        write_memory_file(dir.path(), &mf).unwrap();

        // Verify no .tmp files remain
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        for entry in &entries {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            assert!(
                !name_str.ends_with(".tmp"),
                "leftover tmp file found: {name_str}"
            );
        }
    }

    #[test]
    fn test_delete_memory_file() {
        let dir = tempfile::tempdir().unwrap();
        let mf = MemoryFile::new(
            "Delete Me",
            "Will be deleted",
            MemoryType::User,
            "ephemeral",
        );

        let path = write_memory_file(dir.path(), &mf).unwrap();
        assert!(path.exists());

        delete_memory_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn test_delete_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.md");

        let result = delete_memory_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_nonexistent_file() {
        let result = read_memory_file(Path::new("/tmp/surely_nonexistent_memory_file.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_write_all_memory_types() {
        let dir = tempfile::tempdir().unwrap();
        let types = [
            ("User Mem", MemoryType::User),
            ("Feedback Mem", MemoryType::Feedback),
            ("Project Mem", MemoryType::Project),
            ("Reference Mem", MemoryType::Reference),
        ];

        for (name, mt) in &types {
            let mf = MemoryFile::new(*name, "testing type", *mt, "content");
            let path = write_memory_file(dir.path(), &mf).unwrap();
            let read_back = read_memory_file(&path).unwrap();
            assert_eq!(read_back.memory_type, *mt);
        }
    }

    #[test]
    fn test_frontmatter_format_exact() {
        let mf = MemoryFile::new(
            "Exact Format",
            "Testing exact output",
            MemoryType::User,
            "Body text.",
        );
        let md = mf.to_markdown();

        // Verify the exact format Claude Code expects
        assert!(md.starts_with("---\n"));
        assert!(md.contains("name: Exact Format\n"));
        assert!(md.contains("description: Testing exact output\n"));
        assert!(md.contains("type: user\n"));
        assert!(md.contains("---\n\nBody text."));
    }
}
