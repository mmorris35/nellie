//! Path resolution for Claude Code directories.
//!
//! Claude Code stores its configuration and memory files under
//! `~/.claude/`. This module resolves the canonical paths for:
//!
//! - Project memory: `~/.claude/projects/<sanitized-cwd>/memory/`
//! - Rules: `~/.claude/rules/`
//! - Settings: `~/.claude/settings.json`
//! - Transcripts: `~/.claude/projects/<sanitized-cwd>/`
//!
//! Path sanitization follows Claude Code's convention: the absolute
//! working directory path has its leading `/` stripped, then all
//! remaining `/` characters are replaced with `-`.

use std::path::{Path, PathBuf};

use crate::error::Error;

/// Returns the user's home directory.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined
/// (e.g., `$HOME` is not set).
fn home_dir() -> crate::Result<PathBuf> {
    dirs::home_dir()
        .ok_or_else(|| Error::Internal("unable to determine home directory".to_string()))
}

/// Returns the base Claude Code directory: `~/.claude/`.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn resolve_claude_dir() -> crate::Result<PathBuf> {
    Ok(home_dir()?.join(".claude"))
}

/// Returns the project memory directory for the given working directory.
///
/// The returned path is:
/// `~/.claude/projects/<sanitized-cwd>/memory/`
///
/// # Arguments
///
/// * `cwd` - The absolute working directory of the project.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
///
/// # Examples
///
/// ```rust,ignore
/// # use std::path::Path;
/// # use nellie::claude_code::paths::resolve_project_memory_dir;
/// let dir = resolve_project_memory_dir(
///     Path::new("/home/user/github/nellie-rs"),
/// )?;
/// // ~/.claude/projects/-home-user-github-nellie-rs/memory/
/// ```
pub fn resolve_project_memory_dir(cwd: &Path) -> crate::Result<PathBuf> {
    let sanitized = sanitize_cwd(cwd);
    Ok(resolve_claude_dir()?
        .join("projects")
        .join(sanitized)
        .join("memory"))
}

/// Returns the Claude Code rules directory: `~/.claude/rules/`.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn resolve_rules_dir() -> crate::Result<PathBuf> {
    Ok(resolve_claude_dir()?.join("rules"))
}

/// Returns the path to Claude Code's settings file:
/// `~/.claude/settings.json`.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn resolve_settings_path() -> crate::Result<PathBuf> {
    Ok(resolve_claude_dir()?.join("settings.json"))
}

/// Returns the transcript directory for the given working directory.
///
/// The returned path is:
/// `~/.claude/projects/<sanitized-cwd>/`
///
/// Session transcripts are stored as `<session-id>.jsonl` files
/// within this directory.
///
/// # Arguments
///
/// * `cwd` - The absolute working directory of the project.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn resolve_transcript_dir(cwd: &Path) -> crate::Result<PathBuf> {
    let sanitized = sanitize_cwd(cwd);
    Ok(resolve_claude_dir()?.join("projects").join(sanitized))
}

/// Sanitizes a working directory path into Claude Code's project
/// directory name format.
///
/// The conversion follows Claude Code's convention:
/// 1. Convert the path to a string representation
/// 2. Replace all `/` characters with `-`
///
/// This produces names like `-home-user-github-nellie-rs` from
/// `/home/user/github/nellie-rs`.
///
/// # Arguments
///
/// * `cwd` - The absolute working directory path to sanitize.
///
/// # Examples
///
/// ```rust,ignore
/// # use std::path::Path;
/// # use nellie::claude_code::paths::sanitize_cwd;
/// assert_eq!(
///     sanitize_cwd(Path::new("/home/user/github/nellie-rs")),
///     "-home-user-github-nellie-rs"
/// );
/// ```
pub fn sanitize_cwd(cwd: &Path) -> String {
    let path_str = cwd.to_string_lossy();
    path_str.replace('/', "-")
}

/// State file name for tracking sync/ingest timestamps.
const STATE_FILE: &str = ".nellie-state.json";

/// Returns the path to the Nellie state file for the given project.
///
/// The state file lives in the project memory directory:
/// `~/.claude/projects/<sanitized-cwd>/memory/.nellie-state.json`
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn resolve_state_path(cwd: &Path) -> crate::Result<PathBuf> {
    Ok(resolve_project_memory_dir(cwd)?.join(STATE_FILE))
}

/// Writes a timestamp to the Nellie state file for the given key.
///
/// The state file is a small JSON object with keys like
/// `last_sync_time` and `last_ingest_time` (Unix timestamps).
///
/// # Errors
///
/// Returns an error if the state file cannot be read or written.
pub fn write_state_timestamp(cwd: &Path, key: &str) -> crate::Result<()> {
    let state_path = resolve_state_path(cwd)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Internal(format!("system time error: {e}")))?
        .as_secs();

    // Ensure the parent directory exists
    if let Some(parent) = state_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Internal(format!("create state dir: {e}")))?;
    }

    // Read existing state or start fresh
    let mut state: serde_json::Map<String, serde_json::Value> = if state_path.exists() {
        let content = std::fs::read_to_string(&state_path).unwrap_or_default();
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        serde_json::Map::new()
    };

    state.insert(key.to_string(), serde_json::Value::from(now));

    // Atomic write
    let tmp_path = state_path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(&state)
        .map_err(|e| Error::Internal(format!("JSON serialize error: {e}")))?;
    std::fs::write(&tmp_path, &json)
        .map_err(|e| Error::Internal(format!("write state file: {e}")))?;
    std::fs::rename(&tmp_path, &state_path)
        .map_err(|e| Error::Internal(format!("rename state file: {e}")))?;

    Ok(())
}

/// Reads a timestamp from the Nellie state file for the given key.
///
/// Returns `None` if the state file doesn't exist or the key is missing.
pub fn read_state_timestamp(cwd: &Path, key: &str) -> Option<i64> {
    let state_path = resolve_state_path(cwd).ok()?;
    let content = std::fs::read_to_string(state_path).ok()?;
    let state: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&content).ok()?;
    state.get(key)?.as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_cwd_basic() {
        let result = sanitize_cwd(Path::new("/home/user/github/nellie-rs"));
        assert_eq!(result, "-home-user-github-nellie-rs");
    }

    #[test]
    fn test_sanitize_cwd_root() {
        let result = sanitize_cwd(Path::new("/"));
        assert_eq!(result, "-");
    }

    #[test]
    fn test_sanitize_cwd_deeply_nested() {
        let result = sanitize_cwd(Path::new("/home/user/projects/org/repo/subdir"));
        assert_eq!(result, "-home-user-projects-org-repo-subdir");
    }

    #[test]
    fn test_sanitize_cwd_single_component() {
        let result = sanitize_cwd(Path::new("/tmp"));
        assert_eq!(result, "-tmp");
    }

    #[test]
    fn test_sanitize_cwd_with_hyphens() {
        let result = sanitize_cwd(Path::new("/home/user/my-project"));
        assert_eq!(result, "-home-user-my-project");
    }

    #[test]
    fn test_sanitize_cwd_with_dots() {
        let result = sanitize_cwd(Path::new("/home/user/.config/app"));
        assert_eq!(result, "-home-user-.config-app");
    }

    #[test]
    fn test_resolve_project_memory_dir_structure() {
        // Test that the path has the correct structure
        let result = resolve_project_memory_dir(Path::new("/home/user/github/nellie-rs"));
        // This should succeed on any system with a home directory
        let path = result.expect("home dir should be resolvable");
        let path_str = path.to_string_lossy();

        // Should contain the .claude/projects/ structure
        assert!(
            path_str.contains(".claude/projects/"),
            "path should contain .claude/projects/: {path_str}"
        );
        // Should contain the sanitized cwd
        assert!(
            path_str.contains("-home-user-github-nellie-rs"),
            "path should contain sanitized cwd: {path_str}"
        );
        // Should end with /memory
        assert!(
            path_str.ends_with("/memory"),
            "path should end with /memory: {path_str}"
        );
    }

    #[test]
    fn test_resolve_rules_dir_structure() {
        let result = resolve_rules_dir();
        let path = result.expect("home dir should be resolvable");
        let path_str = path.to_string_lossy();

        assert!(
            path_str.ends_with(".claude/rules"),
            "path should end with .claude/rules: {path_str}"
        );
    }

    #[test]
    fn test_resolve_settings_path_structure() {
        let result = resolve_settings_path();
        let path = result.expect("home dir should be resolvable");
        let path_str = path.to_string_lossy();

        assert!(
            path_str.ends_with(".claude/settings.json"),
            "path should end with .claude/settings.json: \
             {path_str}"
        );
    }

    #[test]
    fn test_resolve_transcript_dir_structure() {
        let result = resolve_transcript_dir(Path::new("/home/user/github/nellie-rs"));
        let path = result.expect("home dir should be resolvable");
        let path_str = path.to_string_lossy();

        assert!(
            path_str.contains(".claude/projects/"),
            "path should contain .claude/projects/: {path_str}"
        );
        assert!(
            path_str.contains("-home-user-github-nellie-rs"),
            "path should contain sanitized cwd: {path_str}"
        );
        // Transcript dir does NOT end with /memory
        assert!(
            !path_str.ends_with("/memory"),
            "transcript path should not end with /memory: \
             {path_str}"
        );
    }

    #[test]
    fn test_resolve_claude_dir_structure() {
        let result = resolve_claude_dir();
        let path = result.expect("home dir should be resolvable");
        let path_str = path.to_string_lossy();

        assert!(
            path_str.ends_with(".claude"),
            "path should end with .claude: {path_str}"
        );
    }

    #[test]
    fn test_project_memory_dir_and_transcript_dir_share_prefix() {
        let cwd = Path::new("/home/user/project");
        let memory_dir = resolve_project_memory_dir(cwd).expect("home dir should be resolvable");
        let transcript_dir = resolve_transcript_dir(cwd).expect("home dir should be resolvable");

        // The memory dir should be a child of the transcript dir
        assert!(
            memory_dir.starts_with(&transcript_dir),
            "memory dir {memory_dir:?} should be under \
             transcript dir {transcript_dir:?}"
        );
    }

    #[test]
    fn test_all_paths_are_absolute() {
        let cwd = Path::new("/home/user/project");
        let paths = vec![
            resolve_claude_dir().expect("home dir should be resolvable"),
            resolve_project_memory_dir(cwd).expect("home dir should be resolvable"),
            resolve_rules_dir().expect("home dir should be resolvable"),
            resolve_settings_path().expect("home dir should be resolvable"),
            resolve_transcript_dir(cwd).expect("home dir should be resolvable"),
        ];

        for path in paths {
            assert!(path.is_absolute(), "path should be absolute: {path:?}");
        }
    }
}
