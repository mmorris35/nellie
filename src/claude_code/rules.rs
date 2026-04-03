//! Conditional rules generator for Claude Code.
//!
//! This module maps Nellie lesson tags to file glob patterns, enabling
//! glob-conditioned rule files that inject relevant lessons into Claude
//! Code's context only when working on matching files. For example, a
//! lesson tagged `"sqlite"` produces rules that activate only when
//! editing storage code.
//!
//! # Tag-to-Glob Mapping
//!
//! [`TagGlobMapper`] maintains a mapping from tag names to file glob
//! patterns. A set of built-in mappings covers common Rust project
//! patterns (storage, server, embeddings, etc.). Additional custom
//! mappings can be added at runtime.
//!
//! Tags that don't match any known mapping receive a fallback glob of
//! `**/*<tag>*` so they still provide some conditional scoping.
//!
//! # Rules File Writer
//!
//! [`write_rule_file`] writes a rule file with YAML frontmatter
//! containing the globs array, formatted for Claude Code's conditional
//! loading system. Each file is named `nellie-{lesson_id_short}.md`
//! so that all Nellie-generated rules are easily identifiable and
//! cleanable.
//!
//! [`clean_stale_rules`] removes Nellie-generated rule files that no
//! longer correspond to active lessons.
//!
//! # Examples
//!
//! ```rust,ignore
//! use nellie::claude_code::rules::TagGlobMapper;
//!
//! let mapper = TagGlobMapper::new();
//! let globs = mapper.tags_to_globs(&["sqlite".into(), "axum".into()]);
//! // ["src/storage/**/*.rs", "**/*sqlite*", "src/server/**/*.rs"]
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::storage::LessonRecord;

/// Maps lesson tags to file glob patterns for conditional rule loading.
///
/// The mapper contains a set of built-in mappings for common tags
/// (database, server, embeddings, etc.) and supports adding custom
/// mappings at runtime. Multiple tags may map to the same glob
/// pattern; the output is deduplicated.
///
/// Tags that don't match any known mapping get a fallback glob of
/// `**/*<tag>*` which provides broad but reasonable conditional
/// scoping.
#[derive(Debug, Clone)]
pub struct TagGlobMapper {
    /// Tag name (lowercase) -> list of glob patterns.
    mappings: HashMap<String, Vec<String>>,
}

impl Default for TagGlobMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl TagGlobMapper {
    /// Creates a new mapper with built-in default mappings.
    ///
    /// The defaults cover common patterns for a Rust project:
    ///
    /// | Tags | Globs |
    /// |------|-------|
    /// | `sqlite`, `rusqlite`, `database`, `db` | `src/storage/**/*.rs`, `**/*sqlite*` |
    /// | `axum`, `http`, `server`, `api`, `rest` | `src/server/**/*.rs` |
    /// | `embedding`, `embeddings`, `onnx`, `ort` | `src/embeddings/**/*.rs` |
    /// | `watcher`, `notify`, `indexer`, `file-watch` | `src/watcher/**/*.rs` |
    /// | `graph`, `petgraph`, `knowledge-graph` | `src/graph/**/*.rs` |
    /// | `cargo`, `rust`, `rustc`, `clippy` | `*.rs`, `Cargo.toml` |
    /// | `git`, `gitignore` | `.gitignore`, `**/*.rs` |
    /// | `config`, `configuration`, `settings` | `src/config/**/*.rs`, `**/*.toml`, `**/*.json` |
    /// | `test`, `testing` | `tests/**/*.rs`, `src/**/*test*` |
    /// | `cli`, `clap`, `command` | `src/main.rs`, `src/cli/**/*.rs` |
    /// | `docker`, `container` | `Dockerfile`, `docker-compose*.yml` |
    /// | `ci`, `github-actions`, `pipeline` | `.github/**/*.yml` |
    /// | `claude`, `claude-code`, `memory` | `src/claude_code/**/*.rs`, `.claude/**/*` |
    /// | `mcp` | `src/server/**/*.rs` |
    /// | `serde`, `json`, `serialization` | `**/*.json`, `**/*.rs` |
    /// | `toml`, `yaml`, `yml` | `**/*.toml`, `**/*.yml`, `**/*.yaml` |
    /// | `error`, `error-handling`, `thiserror`, `anyhow` | `src/error/**/*.rs`, `**/*.rs` |
    /// | `async`, `tokio`, `futures` | `**/*.rs` |
    #[must_use]
    pub fn new() -> Self {
        let mut mappings = HashMap::new();

        // Database / storage
        let storage_globs = vec!["src/storage/**/*.rs".to_string(), "**/*sqlite*".to_string()];
        for tag in &["sqlite", "rusqlite", "database", "db"] {
            mappings.insert((*tag).to_string(), storage_globs.clone());
        }

        // Server / HTTP
        let server_globs = vec!["src/server/**/*.rs".to_string()];
        for tag in &["axum", "http", "server", "api", "rest"] {
            mappings.insert((*tag).to_string(), server_globs.clone());
        }

        // Embeddings
        let embedding_globs = vec!["src/embeddings/**/*.rs".to_string()];
        for tag in &["embedding", "embeddings", "onnx", "ort"] {
            mappings.insert((*tag).to_string(), embedding_globs.clone());
        }

        // File watcher
        let watcher_globs = vec!["src/watcher/**/*.rs".to_string()];
        for tag in &["watcher", "notify", "indexer", "file-watch"] {
            mappings.insert((*tag).to_string(), watcher_globs.clone());
        }

        // Graph
        let graph_globs = vec!["src/graph/**/*.rs".to_string()];
        for tag in &["graph", "petgraph", "knowledge-graph"] {
            mappings.insert((*tag).to_string(), graph_globs.clone());
        }

        // Rust / Cargo (broad)
        let rust_globs = vec!["*.rs".to_string(), "Cargo.toml".to_string()];
        for tag in &["cargo", "rust", "rustc", "clippy"] {
            mappings.insert((*tag).to_string(), rust_globs.clone());
        }

        // Git
        let git_globs = vec![".gitignore".to_string(), "**/*.rs".to_string()];
        for tag in &["git", "gitignore"] {
            mappings.insert((*tag).to_string(), git_globs.clone());
        }

        // Config / settings
        let config_globs = vec![
            "src/config/**/*.rs".to_string(),
            "**/*.toml".to_string(),
            "**/*.json".to_string(),
        ];
        for tag in &["config", "configuration", "settings"] {
            mappings.insert((*tag).to_string(), config_globs.clone());
        }

        // Testing
        let test_globs = vec!["tests/**/*.rs".to_string(), "src/**/*test*".to_string()];
        for tag in &["test", "testing"] {
            mappings.insert((*tag).to_string(), test_globs.clone());
        }

        // CLI
        let cli_globs = vec!["src/main.rs".to_string(), "src/cli/**/*.rs".to_string()];
        for tag in &["cli", "clap", "command"] {
            mappings.insert((*tag).to_string(), cli_globs.clone());
        }

        // Docker
        let docker_globs = vec!["Dockerfile".to_string(), "docker-compose*.yml".to_string()];
        for tag in &["docker", "container"] {
            mappings.insert((*tag).to_string(), docker_globs.clone());
        }

        // CI/CD
        let ci_globs = vec![".github/**/*.yml".to_string()];
        for tag in &["ci", "github-actions", "pipeline"] {
            mappings.insert((*tag).to_string(), ci_globs.clone());
        }

        // Claude Code integration
        let claude_globs = vec![
            "src/claude_code/**/*.rs".to_string(),
            ".claude/**/*".to_string(),
        ];
        for tag in &["claude", "claude-code", "memory"] {
            mappings.insert((*tag).to_string(), claude_globs.clone());
        }

        // MCP
        mappings.insert("mcp".to_string(), server_globs);

        // Serialization
        let serde_globs = vec!["**/*.json".to_string(), "**/*.rs".to_string()];
        for tag in &["serde", "json", "serialization"] {
            mappings.insert((*tag).to_string(), serde_globs.clone());
        }

        // Data formats
        let format_globs = vec![
            "**/*.toml".to_string(),
            "**/*.yml".to_string(),
            "**/*.yaml".to_string(),
        ];
        for tag in &["toml", "yaml", "yml"] {
            mappings.insert((*tag).to_string(), format_globs.clone());
        }

        // Error handling
        let error_globs = vec!["src/error/**/*.rs".to_string(), "**/*.rs".to_string()];
        for tag in &["error", "error-handling", "thiserror", "anyhow"] {
            mappings.insert((*tag).to_string(), error_globs.clone());
        }

        // Async runtime
        let async_globs = vec!["**/*.rs".to_string()];
        for tag in &["async", "tokio", "futures"] {
            mappings.insert((*tag).to_string(), async_globs.clone());
        }

        Self { mappings }
    }

    /// Adds a custom tag-to-glob mapping, overwriting any existing
    /// mapping for that tag.
    ///
    /// Tag names are normalized to lowercase for consistent lookup.
    ///
    /// # Arguments
    ///
    /// * `tag` - The tag name (will be lowercased).
    /// * `globs` - The glob patterns to associate with this tag.
    pub fn add_mapping(&mut self, tag: &str, globs: Vec<String>) {
        self.mappings.insert(tag.to_lowercase(), globs);
    }

    /// Removes a tag-to-glob mapping if it exists.
    ///
    /// Returns `true` if the mapping was present and removed.
    pub fn remove_mapping(&mut self, tag: &str) -> bool {
        self.mappings.remove(&tag.to_lowercase()).is_some()
    }

    /// Returns the glob patterns for a single tag.
    ///
    /// If the tag has no known mapping, returns a fallback glob of
    /// `**/*<tag>*`. Returns `None` only if the tag is empty or
    /// contains only whitespace.
    #[must_use]
    pub fn globs_for_tag(&self, tag: &str) -> Option<Vec<String>> {
        let normalized = tag.trim().to_lowercase();
        if normalized.is_empty() {
            return None;
        }

        self.mappings.get(&normalized).map_or_else(
            || {
                // Fallback: generic glob based on the tag name.
                // Sanitize the tag to prevent path traversal or
                // invalid glob chars.
                let safe_tag = sanitize_tag_for_glob(&normalized);
                if safe_tag.is_empty() {
                    None
                } else {
                    Some(vec![format!("**/*{safe_tag}*")])
                }
            },
            |globs| Some(globs.clone()),
        )
    }

    /// Maps a slice of lesson tags to a deduplicated list of file glob
    /// patterns.
    ///
    /// Each tag is looked up in the mapping table. Known tags produce
    /// specific globs; unknown tags get a fallback glob of
    /// `**/*<tag>*`. The output is deduplicated (preserving first
    /// occurrence order) so that overlapping tags don't produce
    /// duplicate globs.
    ///
    /// Empty tags and whitespace-only tags are silently skipped.
    ///
    /// # Arguments
    ///
    /// * `tags` - The lesson tags to map.
    ///
    /// # Returns
    ///
    /// A `Vec<String>` of deduplicated glob patterns, in the order
    /// they were first encountered.
    #[must_use]
    pub fn tags_to_globs(&self, tags: &[String]) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();

        for tag in tags {
            if let Some(globs) = self.globs_for_tag(tag) {
                for glob in globs {
                    if seen.insert(glob.clone()) {
                        result.push(glob);
                    }
                }
            }
        }

        result
    }

    /// Returns the number of tag mappings currently registered.
    #[must_use]
    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

    /// Returns `true` if a mapping exists for the given tag.
    #[must_use]
    pub fn has_mapping(&self, tag: &str) -> bool {
        self.mappings.contains_key(&tag.to_lowercase())
    }

    /// Returns a reference to all registered mappings.
    #[must_use]
    pub fn mappings(&self) -> &HashMap<String, Vec<String>> {
        &self.mappings
    }
}

/// Convenience function that uses the default mapper to convert tags
/// to globs.
///
/// This is equivalent to `TagGlobMapper::new().tags_to_globs(tags)`.
///
/// # Arguments
///
/// * `tags` - The lesson tags to map.
///
/// # Returns
///
/// A `Vec<String>` of deduplicated glob patterns.
#[must_use]
pub fn tags_to_globs(tags: &[String]) -> Vec<String> {
    TagGlobMapper::new().tags_to_globs(tags)
}

/// Sanitizes a tag string for safe use in a glob pattern.
///
/// Removes characters that could be problematic in glob patterns or
/// file paths: `/`, `\`, `..`, `*`, `?`, `[`, `]`, `{`, `}`.
/// Collapses consecutive hyphens and trims hyphens from edges.
fn sanitize_tag_for_glob(tag: &str) -> String {
    let sanitized: String = tag
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();

    // Remove ".." sequences (path traversal)
    let sanitized = sanitized.replace("..", "");

    // Collapse consecutive hyphens/underscores
    let mut result = String::with_capacity(sanitized.len());
    let mut prev_sep = false;
    for c in sanitized.chars() {
        if c == '-' || c == '_' {
            if !prev_sep {
                result.push(c);
            }
            prev_sep = true;
        } else {
            result.push(c);
            prev_sep = false;
        }
    }

    result
        .trim_matches(|c: char| c == '-' || c == '_')
        .to_string()
}

/// Length of the shortened lesson ID used in rule filenames.
///
/// Lesson IDs have the form `lesson_<16-hex-chars>`. We take the
/// first 8 hex characters of the hash portion for the filename,
/// giving `nellie-<8chars>.md`. This is short enough to be readable
/// while providing 4 billion unique values to avoid collisions.
const SHORT_ID_LEN: usize = 8;

/// Prefix used for all Nellie-generated rule files.
///
/// Files matching `nellie-*.md` in the rules directory are treated
/// as Nellie-managed and eligible for cleanup by
/// [`clean_stale_rules`].
const RULE_FILE_PREFIX: &str = "nellie-";

/// Extracts the short form of a lesson ID for use in filenames.
///
/// Given a lesson ID like `lesson_aa7e7cd91332a55f`, returns
/// `aa7e7cd9` (the first 8 hex characters after the underscore
/// prefix). If the ID does not contain an underscore, the first 8
/// characters of the full ID are used instead.
///
/// # Arguments
///
/// * `lesson_id` - The full lesson ID string.
///
/// # Returns
///
/// A short identifier string suitable for use in filenames.
#[must_use]
pub fn short_lesson_id(lesson_id: &str) -> String {
    let hash_part = lesson_id
        .find('_')
        .map_or(lesson_id, |pos| &lesson_id[pos + 1..]);

    let end = hash_part.len().min(SHORT_ID_LEN);
    hash_part[..end].to_string()
}

/// Generates the rule filename for a lesson.
///
/// The format is `nellie-{short_id}.md`, where `short_id` is the
/// first 8 hex characters of the lesson's ID hash.
///
/// # Arguments
///
/// * `lesson_id` - The full lesson ID string.
///
/// # Returns
///
/// The filename (not a full path) for this lesson's rule file.
#[must_use]
pub fn rule_filename(lesson_id: &str) -> String {
    format!("{RULE_FILE_PREFIX}{}.md", short_lesson_id(lesson_id))
}

/// Formats a globs array as a YAML inline sequence.
///
/// Produces output like `["src/storage/**/*.rs", "**/*sqlite*"]`
/// which is the format Claude Code expects in rule file frontmatter.
fn format_globs_yaml(globs: &[String]) -> String {
    let quoted: Vec<String> = globs.iter().map(|g| format!("\"{g}\"")).collect();
    format!("[{}]", quoted.join(", "))
}

/// Formats the severity tag for display in rule content.
///
/// Returns a bracketed, lowercase severity string (e.g., `[critical]`).
fn format_severity_tag(severity: &str) -> String {
    let s = severity.trim().to_lowercase();
    if s.is_empty() {
        "[info]".to_string()
    } else {
        format!("[{s}]")
    }
}

/// Generates the full markdown content for a rule file.
///
/// The output has YAML frontmatter with the globs array, followed
/// by the lesson title (with severity tag) and content body.
///
/// # Format
///
/// ```text
/// ---
/// globs: ["src/storage/**/*.rs", "**/*sqlite*"]
/// ---
///
/// ## [critical] SQLite WAL Lock Contention
///
/// When using sqlite-vec with WAL mode, ensure...
/// ```
fn format_rule_content(lesson: &LessonRecord, globs: &[String]) -> String {
    use std::fmt::Write;

    let mut out = String::new();

    // YAML frontmatter
    out.push_str("---\n");
    let _ = writeln!(out, "globs: {}", format_globs_yaml(globs));
    out.push_str("---\n\n");

    // Title with severity tag
    let severity_tag = format_severity_tag(&lesson.severity);
    let _ = write!(out, "## {} {}\n\n", severity_tag, lesson.title);

    // Content body
    out.push_str(&lesson.content);

    // Ensure trailing newline
    if !out.ends_with('\n') {
        out.push('\n');
    }

    out
}

/// Writes a glob-conditioned rule file for a lesson.
///
/// The file is written atomically (to a `.tmp` file first, then
/// renamed) to prevent corruption. The directory is created if it
/// does not exist.
///
/// # Arguments
///
/// * `dir` - The directory to write the rule file into
///   (typically `~/.claude/rules/`).
/// * `lesson` - The lesson record to generate a rule from.
/// * `globs` - The glob patterns for conditional loading.
///
/// # Returns
///
/// The full path to the written rule file.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, the
/// temporary file cannot be written, or the atomic rename fails.
pub fn write_rule_file(
    dir: &Path,
    lesson: &LessonRecord,
    globs: &[String],
) -> crate::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    let filename = rule_filename(&lesson.id);
    let final_path = dir.join(&filename);
    let tmp_path = dir.join(format!(".{filename}.tmp"));

    let content = format_rule_content(lesson, globs);

    // Step 1: Write to temporary file
    fs::write(&tmp_path, &content)?;

    // Step 2: Atomic rename
    if let Err(e) = fs::rename(&tmp_path, &final_path) {
        // Clean up the temp file on rename failure
        let _ = fs::remove_file(&tmp_path);
        return Err(crate::error::Error::Io(e));
    }

    Ok(final_path)
}

/// Removes Nellie-generated rule files that are no longer active.
///
/// Scans `dir` for files matching `nellie-*.md` and deletes any
/// whose lesson ID is not in `active_lesson_ids`. This keeps the
/// rules directory clean when lessons are deleted from the Nellie
/// database.
///
/// # Arguments
///
/// * `dir` - The rules directory to clean
///   (typically `~/.claude/rules/`).
/// * `active_lesson_ids` - Lesson IDs that should be kept. These
///   should be full-length IDs (e.g., `lesson_aa7e7cd91332a55f`);
///   the function computes short IDs internally for matching.
///
/// # Returns
///
/// The number of stale rule files removed.
///
/// # Errors
///
/// Returns an error if the directory cannot be read. Individual
/// file deletion errors are silently ignored (the file may have
/// been removed concurrently).
pub fn clean_stale_rules(dir: &Path, active_lesson_ids: &[String]) -> crate::Result<usize> {
    if !dir.exists() {
        return Ok(0);
    }

    // Build the set of active short IDs for fast lookup
    let active_short_ids: std::collections::HashSet<String> = active_lesson_ids
        .iter()
        .map(|id| short_lesson_id(id))
        .collect();

    let mut removed = 0;

    let entries = fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only consider Nellie-generated rule files
        if !name_str.starts_with(RULE_FILE_PREFIX) || !name_str.ends_with(".md") {
            continue;
        }

        // Extract the short ID from the filename:
        // "nellie-aa7e7cd9.md" -> "aa7e7cd9"
        let short_id = &name_str[RULE_FILE_PREFIX.len()..name_str.len() - ".md".len()];

        if !active_short_ids.contains(short_id) {
            // This rule file is stale — remove it
            let _ = fs::remove_file(entry.path());
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- TagGlobMapper::new() defaults ----

    #[test]
    fn test_default_mapper_has_mappings() {
        let mapper = TagGlobMapper::new();
        assert!(
            mapper.mapping_count() > 30,
            "should have many default mappings, got {}",
            mapper.mapping_count()
        );
    }

    #[test]
    fn test_default_mapper_is_same_as_default_trait() {
        let a = TagGlobMapper::new();
        let b = TagGlobMapper::default();
        assert_eq!(a.mapping_count(), b.mapping_count());
    }

    // ---- Storage / database tags ----

    #[test]
    fn test_sqlite_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("sqlite").unwrap();
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
        assert!(globs.contains(&"**/*sqlite*".to_string()));
    }

    #[test]
    fn test_rusqlite_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("rusqlite").unwrap();
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
    }

    #[test]
    fn test_database_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("database").unwrap();
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
    }

    #[test]
    fn test_db_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("db").unwrap();
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
    }

    // ---- Server / HTTP tags ----

    #[test]
    fn test_axum_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("axum").unwrap();
        assert!(globs.contains(&"src/server/**/*.rs".to_string()));
    }

    #[test]
    fn test_http_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("http").unwrap();
        assert!(globs.contains(&"src/server/**/*.rs".to_string()));
    }

    #[test]
    fn test_server_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("server").unwrap();
        assert!(globs.contains(&"src/server/**/*.rs".to_string()));
    }

    #[test]
    fn test_api_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("api").unwrap();
        assert!(globs.contains(&"src/server/**/*.rs".to_string()));
    }

    #[test]
    fn test_rest_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("rest").unwrap();
        assert!(globs.contains(&"src/server/**/*.rs".to_string()));
    }

    // ---- Embedding tags ----

    #[test]
    fn test_embedding_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("embedding").unwrap();
        assert!(globs.contains(&"src/embeddings/**/*.rs".to_string()));
    }

    #[test]
    fn test_onnx_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("onnx").unwrap();
        assert!(globs.contains(&"src/embeddings/**/*.rs".to_string()));
    }

    #[test]
    fn test_ort_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("ort").unwrap();
        assert!(globs.contains(&"src/embeddings/**/*.rs".to_string()));
    }

    // ---- Watcher tags ----

    #[test]
    fn test_watcher_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("watcher").unwrap();
        assert!(globs.contains(&"src/watcher/**/*.rs".to_string()));
    }

    #[test]
    fn test_notify_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("notify").unwrap();
        assert!(globs.contains(&"src/watcher/**/*.rs".to_string()));
    }

    #[test]
    fn test_indexer_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("indexer").unwrap();
        assert!(globs.contains(&"src/watcher/**/*.rs".to_string()));
    }

    #[test]
    fn test_file_watch_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("file-watch").unwrap();
        assert!(globs.contains(&"src/watcher/**/*.rs".to_string()));
    }

    // ---- Graph tags ----

    #[test]
    fn test_graph_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("graph").unwrap();
        assert!(globs.contains(&"src/graph/**/*.rs".to_string()));
    }

    #[test]
    fn test_petgraph_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("petgraph").unwrap();
        assert!(globs.contains(&"src/graph/**/*.rs".to_string()));
    }

    #[test]
    fn test_knowledge_graph_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("knowledge-graph").unwrap();
        assert!(globs.contains(&"src/graph/**/*.rs".to_string()));
    }

    // ---- Rust / Cargo tags ----

    #[test]
    fn test_cargo_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("cargo").unwrap();
        assert!(globs.contains(&"*.rs".to_string()));
        assert!(globs.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn test_rust_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("rust").unwrap();
        assert!(globs.contains(&"*.rs".to_string()));
        assert!(globs.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn test_clippy_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("clippy").unwrap();
        assert!(globs.contains(&"*.rs".to_string()));
    }

    // ---- Git tags ----

    #[test]
    fn test_git_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("git").unwrap();
        assert!(globs.contains(&".gitignore".to_string()));
        assert!(globs.contains(&"**/*.rs".to_string()));
    }

    #[test]
    fn test_gitignore_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("gitignore").unwrap();
        assert!(globs.contains(&".gitignore".to_string()));
    }

    // ---- Config tags ----

    #[test]
    fn test_config_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("config").unwrap();
        assert!(globs.contains(&"src/config/**/*.rs".to_string()));
        assert!(globs.contains(&"**/*.toml".to_string()));
        assert!(globs.contains(&"**/*.json".to_string()));
    }

    #[test]
    fn test_settings_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("settings").unwrap();
        assert!(globs.contains(&"src/config/**/*.rs".to_string()));
    }

    // ---- Testing tags ----

    #[test]
    fn test_test_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("test").unwrap();
        assert!(globs.contains(&"tests/**/*.rs".to_string()));
        assert!(globs.contains(&"src/**/*test*".to_string()));
    }

    // ---- CLI tags ----

    #[test]
    fn test_cli_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("cli").unwrap();
        assert!(globs.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn test_clap_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("clap").unwrap();
        assert!(globs.contains(&"src/main.rs".to_string()));
    }

    // ---- Docker tags ----

    #[test]
    fn test_docker_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("docker").unwrap();
        assert!(globs.contains(&"Dockerfile".to_string()));
        assert!(globs.contains(&"docker-compose*.yml".to_string()));
    }

    // ---- CI tags ----

    #[test]
    fn test_ci_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("ci").unwrap();
        assert!(globs.contains(&".github/**/*.yml".to_string()));
    }

    #[test]
    fn test_github_actions_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("github-actions").unwrap();
        assert!(globs.contains(&".github/**/*.yml".to_string()));
    }

    // ---- Claude Code tags ----

    #[test]
    fn test_claude_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("claude").unwrap();
        assert!(globs.contains(&"src/claude_code/**/*.rs".to_string()));
        assert!(globs.contains(&".claude/**/*".to_string()));
    }

    #[test]
    fn test_claude_code_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("claude-code").unwrap();
        assert!(globs.contains(&"src/claude_code/**/*.rs".to_string()));
    }

    #[test]
    fn test_memory_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("memory").unwrap();
        assert!(globs.contains(&".claude/**/*".to_string()));
    }

    // ---- MCP tag ----

    #[test]
    fn test_mcp_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("mcp").unwrap();
        assert!(globs.contains(&"src/server/**/*.rs".to_string()));
    }

    // ---- Serialization tags ----

    #[test]
    fn test_serde_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("serde").unwrap();
        assert!(globs.contains(&"**/*.json".to_string()));
        assert!(globs.contains(&"**/*.rs".to_string()));
    }

    #[test]
    fn test_json_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("json").unwrap();
        assert!(globs.contains(&"**/*.json".to_string()));
    }

    // ---- Data format tags ----

    #[test]
    fn test_toml_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("toml").unwrap();
        assert!(globs.contains(&"**/*.toml".to_string()));
    }

    #[test]
    fn test_yaml_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("yaml").unwrap();
        assert!(globs.contains(&"**/*.yml".to_string()));
        assert!(globs.contains(&"**/*.yaml".to_string()));
    }

    // ---- Error handling tags ----

    #[test]
    fn test_error_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("error").unwrap();
        assert!(globs.contains(&"src/error/**/*.rs".to_string()));
    }

    #[test]
    fn test_thiserror_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("thiserror").unwrap();
        assert!(globs.contains(&"src/error/**/*.rs".to_string()));
    }

    #[test]
    fn test_anyhow_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("anyhow").unwrap();
        assert!(globs.contains(&"src/error/**/*.rs".to_string()));
    }

    // ---- Async tags ----

    #[test]
    fn test_tokio_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("tokio").unwrap();
        assert!(globs.contains(&"**/*.rs".to_string()));
    }

    #[test]
    fn test_async_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("async").unwrap();
        assert!(globs.contains(&"**/*.rs".to_string()));
    }

    // ---- Fallback / unknown tags ----

    #[test]
    fn test_unknown_tag_gets_fallback_glob() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("some-custom-thing").unwrap();
        assert_eq!(globs, vec!["**/*some-custom-thing*"]);
    }

    #[test]
    fn test_unknown_tag_single_word() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("webpack").unwrap();
        assert_eq!(globs, vec!["**/*webpack*"]);
    }

    #[test]
    fn test_unknown_tag_with_numbers() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("python3").unwrap();
        assert_eq!(globs, vec!["**/*python3*"]);
    }

    // ---- Empty / whitespace tags ----

    #[test]
    fn test_empty_tag_returns_none() {
        let mapper = TagGlobMapper::new();
        assert!(mapper.globs_for_tag("").is_none());
    }

    #[test]
    fn test_whitespace_tag_returns_none() {
        let mapper = TagGlobMapper::new();
        assert!(mapper.globs_for_tag("   ").is_none());
    }

    // ---- Case insensitivity ----

    #[test]
    fn test_case_insensitive_lookup() {
        let mapper = TagGlobMapper::new();
        let lower = mapper.globs_for_tag("sqlite").unwrap();
        let upper = mapper.globs_for_tag("SQLite").unwrap();
        let mixed = mapper.globs_for_tag("SqLiTe").unwrap();
        assert_eq!(lower, upper);
        assert_eq!(lower, mixed);
    }

    #[test]
    fn test_case_insensitive_fallback() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.globs_for_tag("MyCustomTool").unwrap();
        assert_eq!(globs, vec!["**/*mycustomtool*"]);
    }

    // ---- tags_to_globs (multi-tag) ----

    #[test]
    fn test_tags_to_globs_single_tag() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.tags_to_globs(&["sqlite".to_string()]);
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
        assert!(globs.contains(&"**/*sqlite*".to_string()));
    }

    #[test]
    fn test_tags_to_globs_multiple_tags() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.tags_to_globs(&["sqlite".to_string(), "axum".to_string()]);
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
        assert!(globs.contains(&"**/*sqlite*".to_string()));
        assert!(globs.contains(&"src/server/**/*.rs".to_string()));
    }

    #[test]
    fn test_tags_to_globs_deduplicates() {
        let mapper = TagGlobMapper::new();
        // "sqlite" and "database" both produce "src/storage/**/*.rs"
        let globs = mapper.tags_to_globs(&["sqlite".to_string(), "database".to_string()]);
        let storage_count = globs
            .iter()
            .filter(|g| g.as_str() == "src/storage/**/*.rs")
            .count();
        assert_eq!(storage_count, 1, "duplicate globs should be removed");
    }

    #[test]
    fn test_tags_to_globs_preserves_order() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.tags_to_globs(&["sqlite".to_string(), "axum".to_string()]);
        // First tag's globs should come before second tag's globs
        let storage_pos = globs
            .iter()
            .position(|g| g == "src/storage/**/*.rs")
            .unwrap();
        let server_pos = globs
            .iter()
            .position(|g| g == "src/server/**/*.rs")
            .unwrap();
        assert!(
            storage_pos < server_pos,
            "first tag's globs should come first"
        );
    }

    #[test]
    fn test_tags_to_globs_empty_list() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.tags_to_globs(&[]);
        assert!(globs.is_empty());
    }

    #[test]
    fn test_tags_to_globs_skips_empty_tags() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.tags_to_globs(&["".to_string(), "sqlite".to_string(), "  ".to_string()]);
        // Should have sqlite globs but no empty-tag artifacts
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
        assert!(!globs.is_empty());
    }

    #[test]
    fn test_tags_to_globs_mixed_known_and_unknown() {
        let mapper = TagGlobMapper::new();
        let globs = mapper.tags_to_globs(&["sqlite".to_string(), "custom-thing".to_string()]);
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
        assert!(globs.contains(&"**/*custom-thing*".to_string()));
    }

    // ---- Convenience function ----

    #[test]
    fn test_free_function_tags_to_globs() {
        let globs = tags_to_globs(&["sqlite".to_string()]);
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
    }

    // ---- Custom mappings ----

    #[test]
    fn test_add_custom_mapping() {
        let mut mapper = TagGlobMapper::new();
        mapper.add_mapping("mylib", vec!["src/mylib/**/*.rs".to_string()]);
        let globs = mapper.globs_for_tag("mylib").unwrap();
        assert_eq!(globs, vec!["src/mylib/**/*.rs"]);
    }

    #[test]
    fn test_custom_mapping_overrides_default() {
        let mut mapper = TagGlobMapper::new();
        let original = mapper.globs_for_tag("sqlite").unwrap();
        assert!(original.len() > 1);

        mapper.add_mapping("sqlite", vec!["my/custom/path/**".to_string()]);
        let overridden = mapper.globs_for_tag("sqlite").unwrap();
        assert_eq!(overridden, vec!["my/custom/path/**"]);
    }

    #[test]
    fn test_remove_mapping() {
        let mut mapper = TagGlobMapper::new();
        assert!(mapper.has_mapping("sqlite"));
        assert!(mapper.remove_mapping("sqlite"));
        assert!(!mapper.has_mapping("sqlite"));

        // After removal, should get fallback
        let globs = mapper.globs_for_tag("sqlite").unwrap();
        assert_eq!(globs, vec!["**/*sqlite*"]);
    }

    #[test]
    fn test_remove_nonexistent_mapping() {
        let mut mapper = TagGlobMapper::new();
        assert!(!mapper.remove_mapping("nonexistent-tag-xyz"));
    }

    #[test]
    fn test_has_mapping() {
        let mapper = TagGlobMapper::new();
        assert!(mapper.has_mapping("sqlite"));
        assert!(mapper.has_mapping("SQLite")); // case insensitive
        assert!(!mapper.has_mapping("nonexistent-xyz"));
    }

    #[test]
    fn test_custom_mapping_case_insensitive() {
        let mut mapper = TagGlobMapper::new();
        mapper.add_mapping("MyLib", vec!["src/mylib/**".to_string()]);
        let globs = mapper.globs_for_tag("mylib").unwrap();
        assert_eq!(globs, vec!["src/mylib/**"]);
    }

    // ---- sanitize_tag_for_glob ----

    #[test]
    fn test_sanitize_tag_normal() {
        assert_eq!(sanitize_tag_for_glob("sqlite"), "sqlite");
    }

    #[test]
    fn test_sanitize_tag_with_hyphens() {
        assert_eq!(sanitize_tag_for_glob("error-handling"), "error-handling");
    }

    #[test]
    fn test_sanitize_tag_with_slashes() {
        assert_eq!(sanitize_tag_for_glob("path/traversal"), "pathtraversal");
    }

    #[test]
    fn test_sanitize_tag_with_dots() {
        assert_eq!(sanitize_tag_for_glob("v1.2"), "v1.2");
    }

    #[test]
    fn test_sanitize_tag_with_double_dots() {
        assert_eq!(sanitize_tag_for_glob("..etc..passwd"), "etcpasswd");
    }

    #[test]
    fn test_sanitize_tag_with_glob_chars() {
        assert_eq!(sanitize_tag_for_glob("test*?[a]"), "testa");
    }

    #[test]
    fn test_sanitize_tag_with_braces() {
        assert_eq!(sanitize_tag_for_glob("test{a,b}"), "testab");
    }

    #[test]
    fn test_sanitize_tag_empty_after_sanitize() {
        let mapper = TagGlobMapper::new();
        // A tag that becomes empty after sanitization
        assert!(mapper.globs_for_tag("***").is_none());
    }

    #[test]
    fn test_sanitize_tag_consecutive_separators() {
        assert_eq!(sanitize_tag_for_glob("a--b__c"), "a-b_c");
    }

    // ---- Integration: full workflow ----

    #[test]
    fn test_full_workflow_lesson_tags() {
        // Simulate a lesson about SQLite WAL locks
        let tags: Vec<String> = vec!["sqlite".into(), "wal".into(), "database".into()];
        let globs = tags_to_globs(&tags);

        // Should include storage paths
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
        assert!(globs.contains(&"**/*sqlite*".to_string()));
        // "wal" is unknown, should get fallback
        assert!(globs.contains(&"**/*wal*".to_string()));
        // Deduplication: "sqlite" and "database" share storage glob
        let storage_count = globs
            .iter()
            .filter(|g| g.as_str() == "src/storage/**/*.rs")
            .count();
        assert_eq!(storage_count, 1);
    }

    #[test]
    fn test_full_workflow_broad_lesson() {
        // A Rust-general lesson
        let tags: Vec<String> = vec!["rust".into(), "error-handling".into(), "anyhow".into()];
        let globs = tags_to_globs(&tags);
        assert!(globs.contains(&"*.rs".to_string()));
        assert!(globs.contains(&"Cargo.toml".to_string()));
        assert!(globs.contains(&"src/error/**/*.rs".to_string()));
    }

    #[test]
    fn test_full_workflow_custom_mapper() {
        let mut mapper = TagGlobMapper::new();
        // Add project-specific mapping
        mapper.add_mapping("payments", vec!["src/billing/**/*.rs".to_string()]);

        let tags: Vec<String> = vec!["payments".into(), "sqlite".into()];
        let globs = mapper.tags_to_globs(&tags);

        assert!(globs.contains(&"src/billing/**/*.rs".to_string()));
        assert!(globs.contains(&"src/storage/**/*.rs".to_string()));
    }

    #[test]
    fn test_all_default_tags_produce_nonempty_globs() {
        let mapper = TagGlobMapper::new();
        for (tag, globs) in mapper.mappings() {
            assert!(
                !globs.is_empty(),
                "tag '{}' should have at least one glob pattern",
                tag
            );
            for glob in globs {
                assert!(!glob.is_empty(), "tag '{}' has an empty glob pattern", tag);
            }
        }
    }

    // ---- short_lesson_id ----

    #[test]
    fn test_short_lesson_id_standard() {
        assert_eq!(short_lesson_id("lesson_aa7e7cd91332a55f"), "aa7e7cd9");
    }

    #[test]
    fn test_short_lesson_id_different_prefix() {
        assert_eq!(short_lesson_id("custom_1234abcd5678ef90"), "1234abcd");
    }

    #[test]
    fn test_short_lesson_id_no_underscore() {
        assert_eq!(short_lesson_id("aa7e7cd91332a55f"), "aa7e7cd9");
    }

    #[test]
    fn test_short_lesson_id_short_hash() {
        // If the hash portion is shorter than 8 chars, use all of it
        assert_eq!(short_lesson_id("lesson_abc"), "abc");
    }

    #[test]
    fn test_short_lesson_id_empty() {
        assert_eq!(short_lesson_id(""), "");
    }

    #[test]
    fn test_short_lesson_id_underscore_only() {
        assert_eq!(short_lesson_id("_abcdef12"), "abcdef12");
    }

    #[test]
    fn test_short_lesson_id_multiple_underscores() {
        // Takes everything after the first underscore
        assert_eq!(short_lesson_id("lesson_abc_def_12345678"), "abc_def_");
    }

    // ---- rule_filename ----

    #[test]
    fn test_rule_filename_standard() {
        assert_eq!(
            rule_filename("lesson_aa7e7cd91332a55f"),
            "nellie-aa7e7cd9.md"
        );
    }

    #[test]
    fn test_rule_filename_prefix_check() {
        let name = rule_filename("lesson_1234abcd5678ef90");
        assert!(name.starts_with("nellie-"));
        assert!(name.ends_with(".md"));
    }

    #[test]
    fn test_rule_filename_unique_for_different_ids() {
        let a = rule_filename("lesson_aa7e7cd91332a55f");
        let b = rule_filename("lesson_bb8f8de02443b66g");
        assert_ne!(a, b);
    }

    // ---- format_globs_yaml ----

    #[test]
    fn test_format_globs_yaml_single() {
        let globs = vec!["src/storage/**/*.rs".to_string()];
        assert_eq!(format_globs_yaml(&globs), "[\"src/storage/**/*.rs\"]");
    }

    #[test]
    fn test_format_globs_yaml_multiple() {
        let globs = vec!["src/storage/**/*.rs".to_string(), "**/*sqlite*".to_string()];
        assert_eq!(
            format_globs_yaml(&globs),
            "[\"src/storage/**/*.rs\", \"**/*sqlite*\"]"
        );
    }

    #[test]
    fn test_format_globs_yaml_empty() {
        let globs: Vec<String> = vec![];
        assert_eq!(format_globs_yaml(&globs), "[]");
    }

    // ---- format_severity_tag ----

    #[test]
    fn test_format_severity_tag_critical() {
        assert_eq!(format_severity_tag("critical"), "[critical]");
    }

    #[test]
    fn test_format_severity_tag_warning() {
        assert_eq!(format_severity_tag("warning"), "[warning]");
    }

    #[test]
    fn test_format_severity_tag_info() {
        assert_eq!(format_severity_tag("info"), "[info]");
    }

    #[test]
    fn test_format_severity_tag_empty() {
        assert_eq!(format_severity_tag(""), "[info]");
    }

    #[test]
    fn test_format_severity_tag_uppercase() {
        assert_eq!(format_severity_tag("CRITICAL"), "[critical]");
    }

    #[test]
    fn test_format_severity_tag_whitespace() {
        assert_eq!(format_severity_tag("  "), "[info]");
    }

    // ---- format_rule_content ----

    fn make_test_lesson(
        id: &str,
        title: &str,
        content: &str,
        severity: &str,
        tags: &[&str],
    ) -> LessonRecord {
        LessonRecord {
            id: id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            severity: severity.to_string(),
            agent: None,
            repo: None,
            created_at: 0,
            updated_at: 0,
            embedding: None,
        }
    }

    #[test]
    fn test_format_rule_content_basic() {
        let lesson = make_test_lesson(
            "lesson_aa7e7cd91332a55f",
            "SQLite WAL Lock Contention",
            "When using sqlite-vec with WAL mode, ensure...",
            "critical",
            &["sqlite", "wal"],
        );
        let globs = vec!["src/storage/**/*.rs".to_string(), "**/*sqlite*".to_string()];

        let content = format_rule_content(&lesson, &globs);

        // Check frontmatter
        assert!(content.starts_with("---\n"));
        assert!(content.contains("globs: [\"src/storage/**/*.rs\", \"**/*sqlite*\"]"));
        assert!(content.contains("---\n\n"));

        // Check title with severity
        assert!(content.contains("## [critical] SQLite WAL Lock Contention"));

        // Check body
        assert!(content.contains("When using sqlite-vec with WAL mode, ensure..."));

        // Check trailing newline
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn test_format_rule_content_info_severity() {
        let lesson = make_test_lesson(
            "lesson_1234",
            "Code Style Tip",
            "Use descriptive names.",
            "info",
            &["rust"],
        );
        let globs = vec!["*.rs".to_string()];

        let content = format_rule_content(&lesson, &globs);
        assert!(content.contains("## [info] Code Style Tip"));
    }

    #[test]
    fn test_format_rule_content_empty_globs() {
        let lesson = make_test_lesson(
            "lesson_abcd",
            "General Advice",
            "Always test.",
            "warning",
            &[],
        );
        let globs: Vec<String> = vec![];

        let content = format_rule_content(&lesson, &globs);
        assert!(content.contains("globs: []"));
    }

    #[test]
    fn test_format_rule_content_trailing_newline() {
        let lesson = make_test_lesson(
            "lesson_1111",
            "Test",
            "Content without trailing newline",
            "info",
            &[],
        );
        let content = format_rule_content(&lesson, &[]);
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn test_format_rule_content_preserves_existing_newline() {
        let lesson = make_test_lesson(
            "lesson_2222",
            "Test",
            "Content with trailing newline\n",
            "info",
            &[],
        );
        let content = format_rule_content(&lesson, &[]);
        assert!(content.ends_with('\n'));
        assert!(!content.ends_with("\n\n"));
    }

    // ---- write_rule_file ----

    #[test]
    fn test_write_rule_file_creates_file() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let lesson = make_test_lesson(
            "lesson_aa7e7cd91332a55f",
            "SQLite WAL Lock",
            "Ensure WAL mode is properly configured.",
            "critical",
            &["sqlite"],
        );
        let globs = vec!["src/storage/**/*.rs".to_string()];

        let path = write_rule_file(dir.path(), &lesson, &globs).expect("write_rule_file failed");

        assert!(path.exists());
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "nellie-aa7e7cd9.md"
        );
    }

    #[test]
    fn test_write_rule_file_content_format() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let lesson = make_test_lesson(
            "lesson_bb8f8de02443b66g",
            "Axum Route Conflicts",
            "Avoid overlapping route patterns.",
            "warning",
            &["axum", "http"],
        );
        let globs = vec!["src/server/**/*.rs".to_string()];

        let path = write_rule_file(dir.path(), &lesson, &globs).expect("write_rule_file failed");

        let content = fs::read_to_string(&path).expect("failed to read rule file");

        // Verify YAML frontmatter structure
        assert!(content.starts_with("---\n"));
        let parts: Vec<&str> = content.splitn(3, "---\n").collect();
        assert!(parts.len() >= 3, "should have frontmatter delimiters");

        // Verify globs in frontmatter
        assert!(parts[1].contains("globs: [\"src/server/**/*.rs\"]"));

        // Verify content
        assert!(content.contains("## [warning] Axum Route Conflicts"));
        assert!(content.contains("Avoid overlapping route patterns."));
    }

    #[test]
    fn test_write_rule_file_creates_directory() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let nested = dir.path().join("sub").join("dir");

        let lesson = make_test_lesson(
            "lesson_1234abcd5678ef90",
            "Test Lesson",
            "Content here.",
            "info",
            &["test"],
        );
        let globs = vec!["tests/**/*.rs".to_string()];

        let path = write_rule_file(&nested, &lesson, &globs).expect("write_rule_file failed");

        assert!(path.exists());
        assert!(nested.is_dir());
    }

    #[test]
    fn test_write_rule_file_overwrites_existing() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let lesson = make_test_lesson("lesson_aa7e7cd91332a55f", "Test", "Version 1", "info", &[]);

        write_rule_file(dir.path(), &lesson, &[]).expect("first write failed");

        let lesson_v2 = make_test_lesson(
            "lesson_aa7e7cd91332a55f",
            "Test Updated",
            "Version 2",
            "critical",
            &[],
        );

        let path = write_rule_file(dir.path(), &lesson_v2, &[]).expect("second write failed");

        let content = fs::read_to_string(&path).expect("failed to read");
        assert!(content.contains("Version 2"));
        assert!(content.contains("[critical]"));
    }

    #[test]
    fn test_write_rule_file_no_tmp_leftovers() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let lesson = make_test_lesson("lesson_1234abcd", "Test", "Content.", "info", &[]);

        write_rule_file(dir.path(), &lesson, &[]).expect("write failed");

        let entries: Vec<_> = fs::read_dir(dir.path()).expect("read_dir failed").collect();
        for entry in &entries {
            let name = entry.as_ref().expect("entry error").file_name();
            let name_str = name.to_string_lossy();
            assert!(
                !name_str.ends_with(".tmp"),
                "leftover tmp file found: {name_str}"
            );
        }
    }

    #[test]
    fn test_write_rule_file_returns_correct_path() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let lesson = make_test_lesson("lesson_deadbeef12345678", "Test", "Content.", "info", &[]);

        let path = write_rule_file(dir.path(), &lesson, &[]).expect("write failed");

        assert_eq!(path, dir.path().join("nellie-deadbeef.md"));
    }

    // ---- clean_stale_rules ----

    #[test]
    fn test_clean_stale_rules_empty_dir() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        let removed = clean_stale_rules(dir.path(), &[]).expect("clean_stale_rules failed");
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_clean_stale_rules_nonexistent_dir() {
        let dir = Path::new("/tmp/nonexistent-rules-dir-12345");
        let removed = clean_stale_rules(dir, &[]).expect("clean_stale_rules failed");
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_clean_stale_rules_keeps_active() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Write a rule file
        let lesson = make_test_lesson(
            "lesson_aa7e7cd91332a55f",
            "Active Lesson",
            "Content.",
            "critical",
            &["sqlite"],
        );
        let globs = vec!["src/storage/**/*.rs".to_string()];
        write_rule_file(dir.path(), &lesson, &globs).expect("write failed");

        // Clean with the lesson still active
        let active = vec!["lesson_aa7e7cd91332a55f".to_string()];
        let removed = clean_stale_rules(dir.path(), &active).expect("clean failed");

        assert_eq!(removed, 0);
        assert!(dir.path().join("nellie-aa7e7cd9.md").exists());
    }

    #[test]
    fn test_clean_stale_rules_removes_stale() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Write two rule files
        let lesson_a = make_test_lesson(
            "lesson_aa7e7cd91332a55f",
            "Keep This",
            "Content.",
            "critical",
            &[],
        );
        let lesson_b = make_test_lesson(
            "lesson_bb8f8de02443b66g",
            "Remove This",
            "Content.",
            "warning",
            &[],
        );

        write_rule_file(dir.path(), &lesson_a, &[]).expect("write a failed");
        write_rule_file(dir.path(), &lesson_b, &[]).expect("write b failed");

        // Only lesson_a is active
        let active = vec!["lesson_aa7e7cd91332a55f".to_string()];
        let removed = clean_stale_rules(dir.path(), &active).expect("clean failed");

        assert_eq!(removed, 1);
        assert!(dir.path().join("nellie-aa7e7cd9.md").exists());
        assert!(!dir.path().join("nellie-bb8f8de0.md").exists());
    }

    #[test]
    fn test_clean_stale_rules_removes_all_when_empty_active() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        let lesson = make_test_lesson("lesson_1234abcd5678ef90", "Test", "Content.", "info", &[]);
        write_rule_file(dir.path(), &lesson, &[]).expect("write failed");

        let removed = clean_stale_rules(dir.path(), &[]).expect("clean failed");

        assert_eq!(removed, 1);
    }

    #[test]
    fn test_clean_stale_rules_ignores_non_nellie_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Create a non-Nellie rule file
        fs::write(
            dir.path().join("my-custom-rule.md"),
            "---\nglobs: [\"*.rs\"]\n---\n\nCustom rule.",
        )
        .expect("write custom rule failed");

        // Create a Nellie rule file
        let lesson = make_test_lesson("lesson_aa7e7cd91332a55f", "Test", "Content.", "info", &[]);
        write_rule_file(dir.path(), &lesson, &[]).expect("write failed");

        // Clean with no active lessons
        let removed = clean_stale_rules(dir.path(), &[]).expect("clean failed");

        // Only the nellie file should be removed
        assert_eq!(removed, 1);
        assert!(dir.path().join("my-custom-rule.md").exists());
    }

    #[test]
    fn test_clean_stale_rules_ignores_non_md_nellie_prefix() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Create a file that starts with nellie- but isn't .md
        fs::write(dir.path().join("nellie-backup.txt"), "not a rule file").expect("write failed");

        let removed = clean_stale_rules(dir.path(), &[]).expect("clean failed");

        assert_eq!(removed, 0);
        assert!(dir.path().join("nellie-backup.txt").exists());
    }

    // ---- Integration: write + clean workflow ----

    #[test]
    fn test_write_and_clean_workflow() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        // Simulate first sync: write 3 rules
        let lessons = vec![
            make_test_lesson(
                "lesson_1111111111111111",
                "Lesson 1",
                "Content 1.",
                "critical",
                &["sqlite"],
            ),
            make_test_lesson(
                "lesson_2222222222222222",
                "Lesson 2",
                "Content 2.",
                "warning",
                &["axum"],
            ),
            make_test_lesson(
                "lesson_3333333333333333",
                "Lesson 3",
                "Content 3.",
                "info",
                &["rust"],
            ),
        ];

        for lesson in &lessons {
            let globs = tags_to_globs(&lesson.tags);
            write_rule_file(dir.path(), lesson, &globs).expect("write failed");
        }

        // All 3 files should exist
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);

        // Simulate second sync: lesson 2 deleted
        let active = vec![
            "lesson_1111111111111111".to_string(),
            "lesson_3333333333333333".to_string(),
        ];
        let removed = clean_stale_rules(dir.path(), &active).expect("clean failed");
        assert_eq!(removed, 1);

        // Verify correct file was removed
        assert!(dir.path().join("nellie-11111111.md").exists());
        assert!(!dir.path().join("nellie-22222222.md").exists());
        assert!(dir.path().join("nellie-33333333.md").exists());
    }

    #[test]
    fn test_write_rule_file_parseable_frontmatter() {
        // Verify the frontmatter is valid by manually parsing it
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let lesson = make_test_lesson(
            "lesson_abcdef1234567890",
            "Important Rule",
            "Rule body content here.",
            "critical",
            &["sqlite", "database"],
        );
        let globs = vec!["src/storage/**/*.rs".to_string(), "**/*sqlite*".to_string()];

        let path = write_rule_file(dir.path(), &lesson, &globs).expect("write failed");
        let content = fs::read_to_string(&path).expect("read failed");

        // Extract frontmatter between --- delimiters
        let parts: Vec<&str> = content.splitn(3, "---\n").collect();
        assert_eq!(parts.len(), 3, "should have 3 parts");
        assert_eq!(parts[0], "", "first part should be empty");

        let frontmatter = parts[1].trim();
        assert!(
            frontmatter.starts_with("globs: ["),
            "frontmatter should start with globs: ["
        );
        assert!(
            frontmatter.contains("\"src/storage/**/*.rs\""),
            "should contain quoted glob"
        );
    }

    #[test]
    fn test_multiple_writes_same_lesson_idempotent() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let lesson = make_test_lesson("lesson_aa7e7cd91332a55f", "Test", "Content.", "info", &[]);

        // Write twice
        write_rule_file(dir.path(), &lesson, &[]).expect("first write failed");
        write_rule_file(dir.path(), &lesson, &[]).expect("second write failed");

        // Only one file should exist
        let count = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, 1);
    }
}
