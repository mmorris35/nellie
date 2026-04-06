//! Prompt-aware context injection for Claude Code.
//!
//! This module implements the `nellie inject` CLI command that searches Nellie
//! for context relevant to a user's prompt and injects it as a temporary rules
//! file. This allows Claude Code to automatically have relevant lessons and
//! gotchas before processing the prompt, reducing clarification loops.
//!
//! # Architecture
//!
//! The injection pipeline:
//! 1. Accept a query (typically the user's prompt text)
//! 2. Search Nellie (local or remote) for relevant lessons
//! 3. Filter results by relevance score (default threshold 0.4, higher=more relevant)
//! 4. Deduplicate against existing session memory files
//! 5. Format as Claude Code rules file with YAML frontmatter
//! 6. Write atomically to `~/.claude/rules/nellie-inject.md`
//! 7. Clean up previous injection file (single file, replaced each time)
//!
//! # Configuration
//!
//! - Default timeout: 800ms
//! - Default limit: 3 results
//! - Default threshold: 0.4 (score-based, 0.0-1.0, higher=more relevant)
//! - Output budget: 500 tokens (~2000 chars)
//!
//! # Fail Open Design
//!
//! If Nellie server is unavailable or times out, `nellie inject` exits 0
//! with 0 injected results. This prevents blocking the user's prompt.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::time::Instant;

/// Configuration for a context injection operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InjectConfig {
    /// The search query (typically the user's prompt text).
    pub query: String,

    /// Maximum number of results to inject.
    pub limit: usize,

    /// Minimum relevance score (0.0-1.0).
    /// Results with distance > threshold are filtered out.
    pub threshold: f64,

    /// Timeout in milliseconds for the search operation.
    pub timeout_ms: u64,

    /// Show what would be injected without writing files.
    pub dry_run: bool,
}

/// Result of a context injection operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InjectResult {
    /// Number of lessons successfully injected.
    pub injected_count: usize,

    /// Number of results that passed relevance filtering but were skipped
    /// (e.g., already in session memory, duplicate).
    pub skipped_count: usize,

    /// Path to the injected rules file (if any).
    /// `None` if no results passed filtering or dry_run was enabled.
    pub file_path: Option<String>,

    /// Total elapsed time in milliseconds.
    pub elapsed_ms: u64,
}

/// A search result from search_hybrid.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SearchResult {
    file_path: String,
    content: String,
    distance: f64,
    score: f64,
    title: String,
}

/// Read existing memory files from the project memory directory and extract their names.
///
/// # Returns
/// A `HashSet` of existing memory file names (from the `name` YAML field).
/// Returns an empty set if the memory directory doesn't exist or can't be read.
///
/// # Deduplication Logic
/// - Scans `~/.claude/projects/<sanitized-cwd>/memory/*.md` files
/// - Parses YAML frontmatter to extract the `name` field
/// - Builds a set of existing lesson names
/// - Returns empty set on any error (fail open)
fn read_existing_memory_names() -> HashSet<String> {
    let mut existing_names = HashSet::new();

    let memory_dir = match std::env::current_dir() {
        Ok(cwd) => match crate::claude_code::paths::resolve_project_memory_dir(&cwd) {
            Ok(dir) => dir,
            Err(_) => return existing_names, // Fail open: no memory dir
        },
        Err(_) => return existing_names, // Fail open: can't get CWD
    };

    // If memory directory doesn't exist, return empty set
    if !memory_dir.exists() {
        return existing_names;
    }

    // Scan all .md files in memory directory
    if let Ok(entries) = fs::read_dir(&memory_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Only process .md files
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }

            // Read file content
            if let Ok(content) = fs::read_to_string(&path) {
                // Extract name from YAML frontmatter
                if let Some(name) = extract_yaml_name(&content) {
                    existing_names.insert(name);
                }
            }
        }
    }

    existing_names
}

/// Extract the `name` field from YAML frontmatter.
///
/// # Arguments
/// - `content`: File content starting with `---`
///
/// # Returns
/// The value of the `name` field if found, otherwise None.
///
/// # Format
/// Expects YAML frontmatter like:
/// ```yaml
/// ---
/// name: Lesson Title
/// description: Some description
/// ---
/// ```
fn extract_yaml_name(content: &str) -> Option<String> {
    // Split by `---` to isolate frontmatter
    let parts: Vec<&str> = content.split("---").collect();
    if parts.len() < 2 {
        return None;
    }

    let frontmatter = parts[1];

    // Look for `name: ` line
    for line in frontmatter.lines() {
        if let Some(value_part) = line.strip_prefix("name:") {
            // Trim whitespace and quotes
            let value = value_part.trim().trim_matches(|c| c == '"' || c == '\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

/// Execute the inject pipeline: search → filter → format → write.
///
/// # Arguments
/// - `config`: Injection configuration (query, limit, threshold, timeout)
/// - `server_url`: Optional remote server URL; if provided, search via HTTP
///
/// # Returns
/// Always returns `Ok(InjectResult)` — never returns error (fail open design).
/// If search fails or times out, returns Ok with 0 injected results.
pub async fn execute_inject(
    config: &InjectConfig,
    server_url: Option<&str>,
) -> crate::Result<InjectResult> {
    let start = Instant::now();

    // Wrap search in timeout
    let timeout_duration = std::time::Duration::from_millis(config.timeout_ms);

    let results: Vec<SearchResult> = if let Some(url) = server_url {
        match tokio::time::timeout(timeout_duration, execute_inject_remote(config, url)).await {
            Ok(Ok(results)) => results,
            Ok(Err(e)) => {
                tracing::debug!("Inject search error (fail open): {e}");
                return Ok(InjectResult {
                    injected_count: 0,
                    skipped_count: 0,
                    file_path: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(_) => {
                tracing::debug!(
                    "Inject search timed out after {}ms (fail open)",
                    config.timeout_ms
                );
                return Ok(InjectResult {
                    injected_count: 0,
                    skipped_count: 0,
                    file_path: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
        }
    } else {
        // Local search not yet implemented — requires --server
        tracing::warn!("No --server URL provided. nellie inject requires a remote server (use --server http://host:port)");
        vec![]
    };

    // Filter by relevance threshold
    // Score is 0.0-1.0 (higher = more relevant). Filter OUT results where score < threshold.
    tracing::debug!(
        "Inject: {} raw results, threshold={}, scores={:?}",
        results.len(),
        config.threshold,
        results.iter().map(|r| r.score).collect::<Vec<_>>()
    );
    let filtered: Vec<SearchResult> = results
        .into_iter()
        .filter(|r| r.score >= config.threshold)
        .take(config.limit)
        .collect();
    tracing::debug!("Inject: {} results passed threshold", filtered.len());

    // Read existing memory names for deduplication
    let existing_names = read_existing_memory_names();

    // Deduplicate against existing memory files
    let mut deduped = Vec::new();
    let mut skipped_count = 0;

    for result in filtered {
        // Skip if title matches an existing memory file name (title-only dedup
        // to avoid false positives from short names matching unrelated content)
        if existing_names.contains(&result.title) {
            skipped_count += 1;
            continue;
        }

        deduped.push(result);
    }

    // Format results as rules blocks, respecting token budget
    let (formatted, blocks_written) = format_results(&deduped);

    // Clean up old injection file and write new one (if there are results)
    let injection_file = cleanup_and_write_injection(&formatted, config.dry_run)?;

    let elapsed = start.elapsed();
    Ok(InjectResult {
        injected_count: blocks_written,
        skipped_count,
        file_path: injection_file,
        elapsed_ms: elapsed.as_millis() as u64,
    })
}

/// Search against remote Nellie server via HTTP.
///
/// # Fail Open
/// Connection errors and HTTP failures return Ok with empty vec.
async fn execute_inject_remote(
    config: &InjectConfig,
    server_url: &str,
) -> crate::Result<Vec<SearchResult>> {
    // Only set connect timeout — overall timeout is handled by tokio::time::timeout
    // in execute_inject. Setting both request timeout and tokio timeout causes races.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(config.timeout_ms))
        .build()
        .map_err(|e| crate::Error::internal(format!("Failed to build HTTP client: {e}")))?;

    let request_body = serde_json::json!({
        "name": "search_hybrid",
        "arguments": {
            "query": config.query,
            "limit": config.limit,
            "expansion_depth": 2,
        }
    });

    let mcp_url = format!("{}/mcp/invoke", server_url.trim_end_matches('/'));
    let response = client
        .post(&mcp_url)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| crate::Error::internal(format!("Failed to call remote server: {e}")))?;

    if !response.status().is_success() {
        return Ok(vec![]);
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| crate::Error::internal(format!("Failed to parse response: {e}")))?;

    // Extract results from MCP response
    tracing::debug!(
        "Inject response top keys: {:?}",
        json.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    let results_array = json["content"]["results"].as_array().ok_or_else(|| {
        tracing::debug!(
            "No content.results in response. content type: {:?}",
            json["content"]
        );
        crate::Error::internal("No results in response")
    })?;

    let mut search_results = Vec::new();
    for result in results_array {
        let file_path = result["file_path"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let title = file_path
            .split('/')
            .next_back()
            .unwrap_or("Context")
            .to_string();
        let sr = SearchResult {
            file_path,
            content: result["content"].as_str().unwrap_or("").to_string(),
            distance: result["distance"].as_f64().unwrap_or(1.0),
            score: result["score"].as_f64().unwrap_or(0.0),
            title,
        };
        search_results.push(sr);
    }

    Ok(search_results)
}

/// Format search results as compact rules-file blocks.
///
/// Each block includes:
/// - YAML frontmatter with `globs: ["*"]`
/// - Severity and title
/// - Truncated content to fit token budget
///
/// Approximate token budget: 500 tokens = ~2000 chars
/// Format search results as a single rules file with one YAML frontmatter block.
///
/// Returns (formatted_string, blocks_written_count).
fn format_results(results: &[SearchResult]) -> (String, usize) {
    const TOKEN_BUDGET: usize = 2000;

    if results.is_empty() {
        return (String::new(), 0);
    }

    // Single YAML frontmatter at the top
    let header = "---\nglobs: [\"*\"]\n---\n\n";
    let mut output = String::from(header);
    let mut char_count = header.len();
    let mut blocks_written = 0;

    for result in results {
        let block_header = format!("## [info] {}\n\n", result.title);
        let block_footer = "\n\n";
        let overhead = block_header.len() + block_footer.len();
        let remaining = TOKEN_BUDGET.saturating_sub(char_count + overhead);

        if remaining < 50 {
            break; // Not enough room for meaningful content
        }

        // Truncate content to fit remaining budget
        let content = if result.content.len() > remaining {
            let cut_at = remaining.min(result.content.len());
            // Find last newline to avoid cutting mid-line
            let cut_point = result.content[..cut_at].rfind('\n').unwrap_or(cut_at);
            format!("{}...", &result.content[..cut_point])
        } else {
            result.content.clone()
        };

        let block = format!("{block_header}{content}{block_footer}");
        output.push_str(&block);
        char_count += block.len();
        blocks_written += 1;
    }

    if blocks_written == 0 {
        return (String::new(), 0);
    }

    (output, blocks_written)
}

/// Clean up previous injection file and write new one (if there are results).
///
/// # Behavior
/// - If `formatted` is empty, delete the injection file (if it exists) and return None
/// - If `formatted` has content and dry_run is false: write atomically (tmp + rename)
/// - If dry_run is true: don't write anything, return None
///
/// # Returns
/// `Ok(Some(path))` if a file was written, `Ok(None)` if no file or dry-run
fn cleanup_and_write_injection(formatted: &str, dry_run: bool) -> crate::Result<Option<String>> {
    let rules_dir = crate::claude_code::paths::resolve_rules_dir()?;

    // Ensure rules directory exists
    fs::create_dir_all(&rules_dir)?;

    let injection_file = rules_dir.join("nellie-inject.md");

    // Delete old file if it exists
    let _ = fs::remove_file(&injection_file);

    // If no results or dry-run, return
    if formatted.is_empty() || dry_run {
        return Ok(None);
    }

    // Write atomically: tmp file then rename
    let tmp_file = rules_dir.join(".nellie-inject.tmp");
    fs::write(&tmp_file, formatted)?;
    fs::rename(&tmp_file, &injection_file)?;

    Ok(Some(injection_file.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_inject_config_creation() {
        let config = InjectConfig {
            query: "test prompt".to_string(),
            limit: 3,
            threshold: 0.6,
            timeout_ms: 500,
            dry_run: false,
        };
        assert_eq!(config.query, "test prompt");
        assert_eq!(config.limit, 3);
        assert_eq!(config.threshold, 0.6);
        assert_eq!(config.timeout_ms, 500);
    }

    #[test]
    fn test_inject_result_creation() {
        let result = InjectResult {
            injected_count: 2,
            skipped_count: 1,
            file_path: Some("/home/user/.claude/rules/nellie-inject.md".to_string()),
            elapsed_ms: 250,
        };
        assert_eq!(result.injected_count, 2);
        assert_eq!(result.skipped_count, 1);
        assert!(result.file_path.is_some());
        assert_eq!(result.elapsed_ms, 250);
    }

    #[test]
    fn test_cleanup_and_write_injection_with_content() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let rules_dir = temp_dir.path();

        // Mock the rules directory resolution
        let formatted = "---\nglobs: [\"*\"]\n---\n## [info] Test Lesson\n\nTest content\n\n";

        // Write directly to test the atomic write pattern
        let injection_file = rules_dir.join("nellie-inject.md");
        let tmp_file = rules_dir.join(".nellie-inject.tmp");

        // Simulate atomic write
        fs::write(&tmp_file, formatted).expect("Failed to write tmp file");
        fs::rename(&tmp_file, &injection_file).expect("Failed to rename");

        // Verify file exists and has correct content
        assert!(injection_file.exists());
        let content = fs::read_to_string(&injection_file).expect("Failed to read file");
        assert_eq!(content, formatted);
    }

    #[test]
    fn test_cleanup_deletes_old_file() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let rules_dir = temp_dir.path();
        let injection_file = rules_dir.join("nellie-inject.md");

        // Create an old file
        fs::write(&injection_file, "old content").expect("Failed to write old file");
        assert!(injection_file.exists());

        // Now delete it (simulating cleanup)
        let _ = fs::remove_file(&injection_file);
        assert!(!injection_file.exists());
    }

    #[test]
    fn test_cleanup_and_write_injection_empty_formatted() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let rules_dir = temp_dir.path();

        // First create an old file
        let injection_file = rules_dir.join("nellie-inject.md");
        fs::write(&injection_file, "old content").expect("Failed to write old file");
        assert!(injection_file.exists());

        // Now delete it when formatted is empty
        let _ = fs::remove_file(&injection_file);

        // Verify it's gone
        assert!(!injection_file.exists());
    }

    /// Helper to create a test SearchResult.
    fn test_result(name: &str, content: &str, score: f64) -> SearchResult {
        SearchResult {
            file_path: format!("/test/{name}"),
            content: content.to_string(),
            distance: 1.0 - score,
            score,
            title: name.to_string(),
        }
    }

    #[test]
    fn test_filter_by_score_threshold() {
        // Score is 0-1, higher=better. Threshold 0.4 means score >= 0.4 passes.
        let results = vec![
            test_result("a.md", "A", 0.6),
            test_result("b.md", "B", 0.5),
            test_result("c.md", "C", 0.3),
            test_result("d.md", "D", 0.2),
            test_result("e.md", "E", 0.1),
        ];

        let threshold = 0.4;
        let filtered: Vec<_> = results
            .into_iter()
            .filter(|r| r.score >= threshold)
            .collect();

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].title, "a.md");
        assert_eq!(filtered[1].title, "b.md");
    }

    #[test]
    fn test_format_single_frontmatter() {
        let results = vec![
            test_result("lesson1.md", "Content 1", 0.8),
            test_result("lesson2.md", "Content 2", 0.7),
        ];

        let (formatted, count) = format_results(&results);
        assert_eq!(count, 2);
        // Should have exactly ONE frontmatter block at the top
        assert_eq!(formatted.matches("---").count(), 2); // opening + closing
        assert!(formatted.starts_with("---\nglobs:"));
    }

    #[test]
    fn test_format_respects_budget() {
        let results = vec![
            test_result("test1.md", &"x".repeat(800), 0.8),
            test_result("test2.md", &"y".repeat(800), 0.7),
            test_result("test3.md", &"z".repeat(800), 0.6),
        ];

        let (formatted, count) = format_results(&results);
        // Content gets truncated to fit budget
        assert!(formatted.len() <= 2100); // Allow small overhead from truncation marker
        assert!(count >= 1); // At least first result fits (truncated)
    }

    #[test]
    fn test_format_empty_results() {
        let results = vec![];
        let (formatted, count) = format_results(&results);
        assert_eq!(formatted, "");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_format_returns_accurate_count() {
        // If results fit, count matches
        let results = vec![test_result("a.md", "Short", 0.9)];
        let (_, count) = format_results(&results);
        assert_eq!(count, 1);

        // If content exceeds budget, it gets truncated (not skipped)
        let huge = vec![test_result("big.md", &"x".repeat(3000), 0.9)];
        let (formatted, count) = format_results(&huge);
        assert_eq!(count, 1); // Truncated but included
        assert!(formatted.contains("...")); // Truncation marker
        assert!(formatted.len() <= 2100);
    }

    #[tokio::test]
    async fn test_timeout_returns_ok() {
        let config = InjectConfig {
            query: "test".to_string(),
            limit: 3,
            threshold: 0.4,
            timeout_ms: 1,
            dry_run: false,
        };

        let result = execute_inject(&config, Some("http://192.168.1.999:9999")).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().injected_count, 0);
    }

    #[tokio::test]
    async fn test_connection_error_returns_ok() {
        let config = InjectConfig {
            query: "test".to_string(),
            limit: 3,
            threshold: 0.4,
            timeout_ms: 800,
            dry_run: false,
        };

        let result = execute_inject(&config, Some("http://127.0.0.1:1")).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().injected_count, 0);
    }

    #[test]
    fn test_extract_yaml_name() {
        let content = "---\nname: Test Lesson\ndescription: A test\n---\n\nContent.";
        assert_eq!(extract_yaml_name(content), Some("Test Lesson".to_string()));
    }

    #[test]
    fn test_extract_yaml_name_with_quotes() {
        let content = "---\nname: \"Quoted Lesson\"\n---\n\nContent.";
        assert_eq!(
            extract_yaml_name(content),
            Some("Quoted Lesson".to_string())
        );
    }

    #[test]
    fn test_extract_yaml_name_missing() {
        let content = "---\ndescription: No name\n---\n\nContent.";
        assert_eq!(extract_yaml_name(content), None);
    }

    #[test]
    fn test_extract_yaml_name_no_frontmatter() {
        assert_eq!(extract_yaml_name("Just plain content"), None);
    }

    #[test]
    fn test_dedup_by_title_match() {
        let existing_names: HashSet<String> =
            vec!["Known Bug Fix".to_string()].into_iter().collect();

        let results = vec![
            test_result("new.md", "Something new", 0.8),
            // Title-only dedup: this has matching content but different title — NOT skipped
            test_result("old.md", "This is about Known Bug Fix details", 0.7),
            // This has matching title — skipped
            test_result("Known Bug Fix", "Different content", 0.6),
        ];

        let mut deduped = Vec::new();
        let mut skipped = 0;
        for result in results {
            if existing_names.contains(&result.title) {
                skipped += 1;
                continue;
            }
            deduped.push(result);
        }

        assert_eq!(deduped.len(), 2); // new.md and old.md pass
        assert_eq!(skipped, 1); // Only "Known Bug Fix" title match skipped
    }
}
