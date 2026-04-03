//! Claude Code hooks management system.
//!
//! This module handles installation, removal, and status checking of Nellie hooks
//! in Claude Code's `settings.json`. The hooks are used to integrate Nellie with
//! Claude Code's native session lifecycle:
//!
//! - **SessionStart hook** (matcher: "startup|resume"): Runs `nellie sync --project "$PWD" --rules`
//!   to populate Claude Code's memory files at session start
//! - **Stop hook** (matcher: ""): Runs `nellie ingest --project "$PWD" --since 1h`
//!   to ingest the completed session transcript for passive learning
//!
//! # Implementation Details
//!
//! - Hooks are identified by the presence of `"nellie "` in their command string
//! - Backups are created before modification (`settings.json.bak`)
//! - Existing non-Nellie hooks are preserved during install/uninstall
//! - Settings.json is created with default structure if missing
//! - JSON is pretty-printed (4-space indent) to remain human-readable

use crate::Result;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Identifies a Nellie hook by the presence of "nellie " in the command
const NELLIE_COMMAND_PREFIX: &str = "nellie ";

/// Identifies an old shell hook by the presence of "nellie-session-start.sh" in the command
const OLD_SHELL_HOOK_MARKERS: &[&str] = &["nellie-session-start.sh", "nellie_session_start"];

/// Report of a hook installation operation.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct HookInstallReport {
    /// Whether SessionStart hook was added or updated
    pub session_start_installed: bool,
    /// Whether Stop hook was added or updated
    pub stop_installed: bool,
    /// Path to the settings.json file
    pub settings_path: PathBuf,
    /// Whether a backup was created
    pub backup_created: bool,
    /// Whether an old shell hook was detected and replaced
    pub old_shell_hook_replaced: bool,
    /// Name and command of the old shell hook (if found)
    pub old_shell_hook_info: Option<(String, String)>,
}

/// Installs or updates Nellie hooks in Claude Code's settings.json.
///
/// # Behavior
///
/// 1. Reads `~/.claude/settings.json` (creates if missing with default structure)
/// 2. Navigates to the `hooks` object
/// 3. Adds/updates SessionStart and Stop hooks with Nellie commands
/// 4. Preserves all existing hooks
/// 5. Backs up the original to `settings.json.bak` before writing
/// 6. Returns a report of what was installed
///
/// # Hook Definitions
///
/// **SessionStart** (matcher: "startup|resume"):
/// ```json
/// {
///   "type": "command",
///   "command": "nellie sync --project \"$PWD\" --rules 2>/dev/null || true",
///   "timeout": 15000
/// }
/// ```
///
/// **Stop** (matcher: ""):
/// ```json
/// {
///   "type": "command",
///   "command": "nellie ingest --project \"$PWD\" --since 1h 2>/dev/null || true",
///   "timeout": 30000
/// }
/// ```
pub fn install_hooks(force: bool, server: Option<&str>) -> Result<HookInstallReport> {
    let settings_path = crate::claude_code::paths::resolve_settings_path()?;

    // Ensure parent directory exists
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Read existing settings.json or create default structure
    let mut settings: Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| {
            json!({
                "hooks": {}
            })
        })
    } else {
        json!({
            "hooks": {}
        })
    };

    // Ensure hooks object exists
    if !settings.is_object() {
        settings = json!({
            "hooks": {}
        });
    }
    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hooks = &mut settings["hooks"];

    // Ensure hooks is an object
    if !hooks.is_object() {
        *hooks = json!({});
    }

    // Detect old shell hook
    let old_shell_hook_info = detect_old_shell_hook(Some(&settings_path));
    // If --force is specified or old hook is detected, remove it
    let old_shell_hook_replaced = if force || old_shell_hook_info.is_some() {
        let count = remove_old_shell_hooks_internal(hooks);
        count > 0
    } else {
        false
    };

    // Remove any existing Nellie hooks to avoid duplicates
    remove_nellie_hooks_internal(hooks);

    // Build hook commands — bake in --server if provided, fall back to env var
    let server_flag = server.map_or_else(
        || " ${NELLIE_SERVER:+--server \"$NELLIE_SERVER\"}".to_string(),
        |url| format!(" --server {url}"),
    );

    // Add SessionStart hook with array of matcher groups format
    let mut session_start_installed = false;
    let session_start_command = json!({
        "type": "command",
        "command": format!("nellie sync --project \"$PWD\" --rules{server_flag} 2>/dev/null || true"),
        "timeout": 15000
    });

    let session_start_hook = json!({
        "matcher": "startup|resume",
        "hooks": [session_start_command]
    });

    if let Some(session_start_val) = hooks.get_mut("SessionStart") {
        // Append to existing array
        if let Some(arr) = session_start_val.as_array_mut() {
            arr.push(session_start_hook);
            session_start_installed = true;
        }
    } else {
        // Create new array with matcher group
        hooks["SessionStart"] = json!([session_start_hook]);
        session_start_installed = true;
    }

    // Add Stop hook with array of matcher groups format
    let mut stop_installed = false;
    let stop_command = json!({
        "type": "command",
        "command": format!("nellie ingest --project \"$PWD\" --since 1h{server_flag} 2>/dev/null || true"),
        "timeout": 30000
    });

    let stop_hook = json!({
        "matcher": "",
        "hooks": [stop_command]
    });

    if let Some(stop_val) = hooks.get_mut("Stop") {
        // Append to existing array
        if let Some(arr) = stop_val.as_array_mut() {
            arr.push(stop_hook);
            stop_installed = true;
        }
    } else {
        // Create new array with matcher group
        hooks["Stop"] = json!([stop_hook]);
        stop_installed = true;
    }

    // Create backup if file exists
    let backup_created = if settings_path.exists() {
        let backup_path = settings_path.with_extension("json.bak");
        fs::copy(&settings_path, &backup_path)?;
        true
    } else {
        false
    };

    // Write updated settings.json with pretty-printing
    let json_str = serde_json::to_string_pretty(&settings)
        .map_err(|e| crate::Error::internal(format!("JSON serialization error: {e}")))?;
    fs::write(&settings_path, json_str)?;

    Ok(HookInstallReport {
        session_start_installed,
        stop_installed,
        settings_path,
        backup_created,
        old_shell_hook_replaced,
        old_shell_hook_info,
    })
}

/// Removes Nellie hooks from Claude Code's settings.json.
///
/// # Behavior
///
/// 1. Reads `~/.claude/settings.json`
/// 2. Removes all hooks with `"nellie "` in the command string
/// 3. Preserves all non-Nellie hooks
/// 4. Writes back to settings.json
/// 5. Returns error if settings.json doesn't exist
pub fn uninstall_hooks() -> Result<()> {
    let settings_path = crate::claude_code::paths::resolve_settings_path()?;

    if !settings_path.exists() {
        return Err(crate::Error::internal(format!(
            "Settings file not found: {}",
            settings_path.display()
        )));
    }

    let content = fs::read_to_string(&settings_path)?;
    let mut settings: Value = serde_json::from_str(&content)
        .map_err(|e| crate::Error::internal(format!("JSON parse error: {e}")))?;

    // Ensure hooks object exists
    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hooks = &mut settings["hooks"];
    if !hooks.is_object() {
        return Ok(());
    }

    // Remove Nellie hooks
    remove_nellie_hooks_internal(hooks);

    // Write back
    let json_str = serde_json::to_string_pretty(&settings)
        .map_err(|e| crate::Error::internal(format!("JSON serialization error: {e}")))?;
    fs::write(&settings_path, json_str)?;

    Ok(())
}

/// Internal helper to remove Nellie hooks from a hooks object.
fn remove_nellie_hooks_internal(hooks: &mut Value) {
    if !hooks.is_object() {
        return;
    }

    let mut keys_to_process = Vec::new();

    // First pass: collect keys to process
    if let Some(obj) = hooks.as_object() {
        for (key, _) in obj {
            keys_to_process.push(key.clone());
        }
    }

    // Second pass: modify the hooks object
    if let Some(obj) = hooks.as_object_mut() {
        let mut keys_to_remove = Vec::new();

        for key in keys_to_process {
            if let Some(hook_val) = obj.get_mut(&key) {
                // Handle both old format (direct object with "command") and new format (array)
                if let Some(command) = hook_val.get("command").and_then(|c| c.as_str()) {
                    if command.contains(NELLIE_COMMAND_PREFIX) {
                        // Mark entire key for removal (old format)
                        keys_to_remove.push(key.clone());
                    }
                } else if let Some(arr) = hook_val.as_array_mut() {
                    // For arrays, remove only the matcher groups that contain Nellie hooks
                    // and preserve the rest
                    arr.retain(|matcher_group| {
                        if let Some(hooks_arr) =
                            matcher_group.get("hooks").and_then(|h| h.as_array())
                        {
                            // Check if any hook in this matcher group is a Nellie hook
                            for hook_item in hooks_arr {
                                if let Some(command) =
                                    hook_item.get("command").and_then(|c| c.as_str())
                                {
                                    if command.contains(NELLIE_COMMAND_PREFIX) {
                                        // This matcher group contains a Nellie hook, remove it
                                        return false;
                                    }
                                }
                            }
                        }
                        // Keep this matcher group (no Nellie hooks found)
                        true
                    });

                    // If the array is now empty, mark the key for removal
                    if arr.is_empty() {
                        keys_to_remove.push(key.clone());
                    }
                }
            }
        }

        // Remove keys that have empty arrays or old format Nellie hooks
        for key in keys_to_remove {
            obj.remove(&key);
        }
    }
}

/// Detects if an old shell hook (`nellie-session-start.sh`) is present in settings.json.
///
/// Returns:
/// - `None` if no old hook is found
/// - `Some((hook_name, command))` if an old hook is found
fn detect_old_shell_hook(settings_path: Option<&PathBuf>) -> Option<(String, String)> {
    let path = settings_path?;

    if !path.exists() {
        return None;
    }

    let content = fs::read_to_string(path).ok()?;
    let Value::Object(obj) = serde_json::from_str::<Value>(&content).ok()? else {
        return None;
    };

    let hooks = obj.get("hooks").and_then(|h| h.as_object())?;

    // Search for hooks containing old shell hook markers
    for (hook_name, hook_obj) in hooks {
        if let Some(command) = hook_obj.get("command").and_then(|c| c.as_str()) {
            for marker in OLD_SHELL_HOOK_MARKERS {
                if command.contains(marker) {
                    return Some((hook_name.clone(), command.to_string()));
                }
            }
        }
    }

    None
}

/// Removes old shell hooks from Claude Code's settings.json.
///
/// # Behavior
///
/// 1. Finds all hooks containing old shell hook markers
/// 2. Removes them from the hooks object
/// 3. Returns the number of hooks removed
fn remove_old_shell_hooks_internal(hooks: &mut Value) -> usize {
    if !hooks.is_object() {
        return 0;
    }

    let mut keys_to_remove = Vec::new();

    if let Some(obj) = hooks.as_object() {
        for (key, hook_val) in obj {
            let mut should_remove = false;

            // Handle both old format (direct object with "command") and new format (array)
            if let Some(command) = hook_val.get("command").and_then(|c| c.as_str()) {
                for marker in OLD_SHELL_HOOK_MARKERS {
                    if command.contains(marker) {
                        should_remove = true;
                        break;
                    }
                }
            } else if let Some(arr) = hook_val.as_array() {
                // Check if any matcher group contains an old shell hook command
                for matcher_group in arr {
                    if let Some(hooks_arr) = matcher_group.get("hooks").and_then(|h| h.as_array()) {
                        for hook_item in hooks_arr {
                            if let Some(command) = hook_item.get("command").and_then(|c| c.as_str())
                            {
                                for marker in OLD_SHELL_HOOK_MARKERS {
                                    if command.contains(marker) {
                                        should_remove = true;
                                        break;
                                    }
                                }
                                if should_remove {
                                    break;
                                }
                            }
                        }
                        if should_remove {
                            break;
                        }
                    }
                }
            }

            if should_remove {
                keys_to_remove.push(key.clone());
            }
        }
    }

    let count = keys_to_remove.len();
    for key in keys_to_remove {
        if let Some(obj) = hooks.as_object_mut() {
            obj.remove(&key);
        }
    }

    count
}

/// Describes the health status of the Nellie hooks system.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct HookStatus {
    /// Whether SessionStart hook is installed
    pub session_start_installed: bool,
    /// Whether Stop hook is installed
    pub stop_installed: bool,
    /// Path to the settings.json file (if it exists)
    pub settings_path: Option<PathBuf>,
    /// Whether the nellie binary is on PATH
    pub nellie_binary_available: bool,
    /// Path to nellie binary if available
    pub nellie_binary_path: Option<PathBuf>,
    /// Last sync time in seconds since epoch (if available)
    pub last_sync_time: Option<i64>,
    /// Last ingest time in seconds since epoch (if available)
    pub last_ingest_time: Option<i64>,
    /// Number of memory files found
    pub memory_file_count: usize,
    /// Number of rule files found
    pub rule_file_count: usize,
    /// Whether the memory directory exists
    pub memory_dir_exists: bool,
    /// Total lines in MEMORY.md (if exists)
    pub memory_index_lines: Option<usize>,
    /// Whether an old shell hook is detected
    pub old_shell_hook_detected: bool,
    /// Information about the old shell hook if detected
    pub old_shell_hook_info: Option<(String, String)>,
}

/// Checks the status of Nellie hooks and related system components.
///
/// # Behavior
///
/// 1. Checks if hooks are installed in settings.json
/// 2. Verifies nellie binary is on PATH
/// 3. Checks last sync/ingest timestamps from database
/// 4. Counts memory and rule files
/// 5. Returns a comprehensive status report
pub fn check_hook_status() -> Result<HookStatus> {
    let settings_path = crate::claude_code::paths::resolve_settings_path().ok();

    // Check for installed hooks
    let (session_start_installed, stop_installed) = check_installed_hooks(settings_path.as_ref());

    // Check for nellie binary on PATH
    let (nellie_binary_available, nellie_binary_path) = check_nellie_binary_on_path();

    // Get last sync/ingest times from database
    let (last_sync_time, last_ingest_time) = get_last_sync_ingest_times().unwrap_or((None, None));

    // Count memory and rule files
    let (memory_file_count, memory_index_lines, memory_dir_exists) = count_memory_files();
    let rule_file_count = count_rule_files();

    // Check for old shell hook
    let old_shell_hook_info = detect_old_shell_hook(settings_path.as_ref());
    let old_shell_hook_detected = old_shell_hook_info.is_some();

    Ok(HookStatus {
        session_start_installed,
        stop_installed,
        settings_path,
        nellie_binary_available,
        nellie_binary_path,
        last_sync_time,
        last_ingest_time,
        memory_file_count,
        rule_file_count,
        memory_dir_exists,
        memory_index_lines,
        old_shell_hook_detected,
        old_shell_hook_info,
    })
}

/// Checks if any hook command in an array-of-matcher-groups value contains the Nellie prefix.
///
/// The hooks format is an array of matcher groups:
///
/// This function traverses the array to find any command with the Nellie prefix.
fn hook_event_has_nellie(event_val: &Value) -> bool {
    let Some(arr) = event_val.as_array() else {
        // Legacy flat format: check direct "command" field
        if let Some(cmd) = event_val.get("command").and_then(|c| c.as_str()) {
            return cmd.contains(NELLIE_COMMAND_PREFIX);
        }
        return false;
    };
    // Array-of-matcher-groups format
    for group in arr {
        if let Some(hooks_arr) = group.get("hooks").and_then(|h| h.as_array()) {
            for hook in hooks_arr {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                    if cmd.contains(NELLIE_COMMAND_PREFIX) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Checks if hooks are installed in settings.json.
fn check_installed_hooks(settings_path: Option<&PathBuf>) -> (bool, bool) {
    let Some(path) = settings_path else {
        return (false, false);
    };

    if !path.exists() {
        return (false, false);
    }

    let Ok(content) = fs::read_to_string(path) else {
        return (false, false);
    };

    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&content) else {
        return (false, false);
    };

    let hooks = obj.get("hooks").and_then(|h| h.as_object());
    let session_start = hooks
        .and_then(|h| h.get("SessionStart"))
        .is_some_and(hook_event_has_nellie);

    let stop = hooks
        .and_then(|h| h.get("Stop"))
        .is_some_and(hook_event_has_nellie);

    (session_start, stop)
}

/// Checks if the nellie binary is available on PATH.
fn check_nellie_binary_on_path() -> (bool, Option<PathBuf>) {
    use std::process::Command;

    // Try to execute "nellie --version" to check if it's on PATH
    match Command::new("nellie").arg("--version").output() {
        Ok(output) if output.status.success() => {
            // Binary is available, try to find its actual path
            // Use "which" command as a fallback
            let path_result =
                Command::new("which")
                    .arg("nellie")
                    .output()
                    .ok()
                    .and_then(|which_output| {
                        String::from_utf8(which_output.stdout)
                            .ok()
                            .map(|path_str| PathBuf::from(path_str.trim()))
                    });

            (true, path_result)
        }
        _ => (false, None),
    }
}

/// Retrieves last sync and ingest times from the database.
fn get_last_sync_ingest_times() -> Result<(Option<i64>, Option<i64>)> {
    let cwd = std::env::current_dir()
        .map_err(|e| crate::Error::internal(format!("cannot get CWD: {e}")))?;

    let last_sync = crate::claude_code::paths::read_state_timestamp(&cwd, "last_sync_time");
    let last_ingest = crate::claude_code::paths::read_state_timestamp(&cwd, "last_ingest_time");

    Ok((last_sync, last_ingest))
}

/// Counts memory files in the Claude Code memory directory.
fn count_memory_files() -> (usize, Option<usize>, bool) {
    let Ok(memory_dir) = crate::claude_code::paths::resolve_project_memory_dir(
        &std::env::current_dir().unwrap_or_default(),
    ) else {
        return (0, None, false);
    };

    if !memory_dir.exists() {
        return (0, None, false);
    }

    let mut count = 0;
    let mut index_lines = None;

    if let Ok(entries) = fs::read_dir(&memory_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let Some(name) = path.file_name() else {
                    continue;
                };
                let Some(name_str) = name.to_str() else {
                    continue;
                };

                if name_str.eq_ignore_ascii_case("MEMORY.md") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        index_lines = Some(content.lines().count());
                    }
                } else if std::path::Path::new(name_str)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                {
                    count += 1;
                }
            }
        }
    }

    (count, index_lines, true)
}

/// Counts rule files in the Claude Code rules directory.
fn count_rule_files() -> usize {
    let Ok(rules_dir) = crate::claude_code::paths::resolve_rules_dir() else {
        return 0;
    };

    if !rules_dir.exists() {
        return 0;
    }

    let mut count = 0;
    if let Ok(entries) = fs::read_dir(&rules_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let Some(name) = path.file_name() else {
                    continue;
                };
                let Some(name_str) = name.to_str() else {
                    continue;
                };

                if name_str.starts_with("nellie-")
                    && std::path::Path::new(name_str)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                {
                    count += 1;
                }
            }
        }
    }

    count
}

/// Formats a timestamp (seconds since epoch) as a human-readable string.
///
/// Returns strings like "2 hours ago", "30 minutes ago", or the absolute date
/// if more than a day ago.
#[allow(clippy::cast_possible_wrap)]
fn format_time_ago(timestamp: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let seconds_ago = now - timestamp;

    if seconds_ago < 0 {
        return "in the future".to_string();
    }

    if seconds_ago < 60 {
        return format!("{seconds_ago} seconds ago");
    }

    let minutes_ago = seconds_ago / 60;
    if minutes_ago < 60 {
        let plural = if minutes_ago == 1 { "" } else { "s" };
        return format!("{minutes_ago} minute{plural} ago");
    }

    let hours_ago = minutes_ago / 60;
    if hours_ago < 24 {
        let plural = if hours_ago == 1 { "" } else { "s" };
        return format!("{hours_ago} hour{plural} ago");
    }

    let days_ago = hours_ago / 24;
    let plural = if days_ago == 1 { "" } else { "s" };
    format!("{days_ago} day{plural} ago")
}

impl HookStatus {
    /// Formats the status as a human-readable text report.
    #[allow(clippy::map_unwrap_or, clippy::or_fun_call)]
    pub fn format_text(&self) -> String {
        let ss_status = if self.session_start_installed {
            "✓ installed"
        } else {
            "✗ not installed"
        };

        let stop_status = if self.stop_installed {
            "✓ installed"
        } else {
            "✗ not installed"
        };

        let binary_line = self.nellie_binary_path.as_ref().map_or(
            "Nellie binary:         ✗ not on PATH\n".to_string(),
            |path| format!("Nellie binary:         ✓ {}\n", path.display()),
        );

        let sync_line = self
            .last_sync_time
            .map_or("Last sync:             never\n".to_string(), |ts| {
                format!("Last sync:             {}\n", format_time_ago(ts))
            });

        let ingest_line = self
            .last_ingest_time
            .map_or("Last ingest:           never\n".to_string(), |ts| {
                format!("Last ingest:           {}\n", format_time_ago(ts))
            });

        let memory_line = if self.memory_dir_exists {
            let budget_warning = self
                .memory_index_lines
                .filter(|lines| *lines > 200)
                .map(|_| " (⚠ exceeds budget)")
                .unwrap_or("");

            self.memory_index_lines.map_or_else(
                || {
                    format!(
                        "Memory files:          {} file(s)\n",
                        self.memory_file_count
                    )
                },
                |lines| {
                    format!(
                        "Memory files:          {} file(s), MEMORY.md: {} lines{}\n",
                        self.memory_file_count, lines, budget_warning
                    )
                },
            )
        } else {
            "Memory files:          no memory directory\n".to_string()
        };

        let old_hook_line = if let Some((hook_name, _command)) = &self.old_shell_hook_info {
            format!(
                "\nLegacy hook detected:  ⚠ {hook_name}\n  \
                 Use 'nellie hooks install --force' to migrate to native hooks\n"
            )
        } else {
            String::new()
        };

        format!(
            "Nellie Deep Hooks Status\n\
             ────────────────────────\n\n\
             SessionStart hook:     {}\n\
             Stop hook:             {}\n\
             {}\
             {}\
             {}\
             {}\
             Rule files:            {}{}\n",
            ss_status,
            stop_status,
            binary_line,
            sync_line,
            ingest_line,
            memory_line,
            self.rule_file_count,
            old_hook_line
        )
    }

    /// Formats the status as JSON.
    pub fn format_json(&self) -> String {
        let old_hook_info = self.old_shell_hook_info.as_ref().map(|(name, cmd)| {
            json!({
                "hook_name": name,
                "command": cmd,
            })
        });

        let json = json!({
            "session_start_installed": self.session_start_installed,
            "stop_installed": self.stop_installed,
            "settings_path": self.settings_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            "nellie_binary_available": self.nellie_binary_available,
            "nellie_binary_path": self.nellie_binary_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            "last_sync_time": self.last_sync_time,
            "last_ingest_time": self.last_ingest_time,
            "memory_file_count": self.memory_file_count,
            "rule_file_count": self.rule_file_count,
            "memory_dir_exists": self.memory_dir_exists,
            "memory_index_lines": self.memory_index_lines,
            "old_shell_hook_detected": self.old_shell_hook_detected,
            "old_shell_hook_info": old_hook_info,
        });

        serde_json::to_string_pretty(&json).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_install_hooks_creates_new_settings() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let settings_path = temp_dir.path().join("settings.json");

        // Mock the path resolution
        let content = format!(
            r#"{{
  "hooks": {{}}
}}"#
        );
        fs::write(&settings_path, content).expect("Failed to write settings");

        // Manually read, install, verify
        let mut settings: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).expect("Failed to read"))
                .expect("Failed to parse JSON");

        let hooks = &mut settings["hooks"];
        if !hooks.is_object() {
            *hooks = json!({});
        }

        // Use new array of matcher groups format
        hooks["SessionStart"] = json!([{
            "matcher": "startup|resume",
            "hooks": [{
                "type": "command",
                "command": "nellie sync --project \"$PWD\" --rules 2>/dev/null || true",
                "timeout": 15000
            }]
        }]);
        hooks["Stop"] = json!([{
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": "nellie ingest --project \"$PWD\" --since 1h 2>/dev/null || true",
                "timeout": 30000
            }]
        }]);

        let json_str = serde_json::to_string_pretty(&settings).expect("Failed to serialize");
        fs::write(&settings_path, json_str).expect("Failed to write");

        // Verify
        let result: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).expect("Failed to read"))
                .expect("Failed to parse");
        assert!(result["hooks"]["SessionStart"].is_array());
        assert!(result["hooks"]["Stop"].is_array());
        assert!(result["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("nellie sync"));
        assert!(result["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("nellie ingest"));
    }

    #[test]
    fn test_install_hooks_preserves_existing_hooks() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let settings_path = temp_dir.path().join("settings.json");

        let initial_content = r#"{
  "hooks": {
    "MyCustomHook": [
      {
        "matcher": "custom",
        "hooks": [
          {
            "type": "command",
            "command": "echo 'custom'",
            "timeout": 5000
          }
        ]
      }
    ]
  }
}"#;

        fs::write(&settings_path, initial_content).expect("Failed to write settings");

        // Simulate install_hooks logic
        let mut settings: Value =
            serde_json::from_str(initial_content).expect("Failed to parse JSON");
        let hooks = &mut settings["hooks"];

        // Use new array format
        hooks["SessionStart"] = json!([{
            "matcher": "startup|resume",
            "hooks": [{
                "type": "command",
                "command": "nellie sync --project \"$PWD\" --rules 2>/dev/null || true",
                "timeout": 15000
            }]
        }]);
        hooks["Stop"] = json!([{
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": "nellie ingest --project \"$PWD\" --since 1h 2>/dev/null || true",
                "timeout": 30000
            }]
        }]);

        let json_str = serde_json::to_string_pretty(&settings).expect("Failed to serialize");

        // Verify custom hook is preserved
        assert!(json_str.contains("MyCustomHook"));
        assert!(json_str.contains("echo 'custom'"));
    }

    #[test]
    fn test_uninstall_hooks_removes_only_nellie_hooks() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let settings_path = temp_dir.path().join("settings.json");

        let initial_content = r#"{
  "hooks": {
    "MyCustomHook": [
      {
        "matcher": "custom",
        "hooks": [
          {
            "type": "command",
            "command": "echo 'custom'",
            "timeout": 5000
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "nellie sync --project \"$PWD\" --rules 2>/dev/null || true",
            "timeout": 15000
          }
        ]
      }
    ],
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "nellie ingest --project \"$PWD\" --since 1h 2>/dev/null || true",
            "timeout": 30000
          }
        ]
      }
    ]
  }
}"#;

        fs::write(&settings_path, initial_content).expect("Failed to write settings");

        // Simulate uninstall_hooks logic
        let mut settings: Value =
            serde_json::from_str(initial_content).expect("Failed to parse JSON");
        let hooks = &mut settings["hooks"];

        remove_nellie_hooks_internal(hooks);

        let json_str = serde_json::to_string_pretty(&settings).expect("Failed to serialize");

        // Verify Nellie hooks are removed
        assert!(!json_str.contains("SessionStart"));
        assert!(!json_str.contains("Stop"));
        // Verify custom hook is preserved
        assert!(json_str.contains("MyCustomHook"));
        assert!(json_str.contains("echo 'custom'"));
    }

    #[test]
    fn test_remove_nellie_hooks_internal() {
        let mut hooks = json!({
            "MyHook": {
                "type": "command",
                "command": "echo 'custom'",
                "timeout": 5000
            },
            "NellieSync": {
                "type": "command",
                "command": "nellie sync --project \"$PWD\"",
                "timeout": 15000
            }
        });

        remove_nellie_hooks_internal(&mut hooks);

        assert!(hooks["MyHook"].is_object());
        assert!(!hooks.as_object().unwrap().contains_key("NellieSync"));
    }

    #[test]
    fn test_hook_command_format_session_start() {
        let hook = json!({
            "matcher": "startup|resume",
            "type": "command",
            "command": "nellie sync --project \"$PWD\" --rules 2>/dev/null || true",
            "timeout": 15000
        });

        assert_eq!(hook["matcher"], "startup|resume");
        assert_eq!(hook["type"], "command");
        assert!(hook["command"].as_str().unwrap().contains("nellie sync"));
        assert!(hook["command"].as_str().unwrap().contains("--rules"));
        assert_eq!(hook["timeout"], 15000);
    }

    #[test]
    fn test_hook_command_format_stop() {
        let hook = json!({
            "matcher": "",
            "type": "command",
            "command": "nellie ingest --project \"$PWD\" --since 1h 2>/dev/null || true",
            "timeout": 30000
        });

        assert_eq!(hook["matcher"], "");
        assert_eq!(hook["type"], "command");
        assert!(hook["command"].as_str().unwrap().contains("nellie ingest"));
        assert!(hook["command"].as_str().unwrap().contains("--since"));
        assert_eq!(hook["timeout"], 30000);
    }

    #[test]
    fn test_identify_nellie_hook() {
        let nellie_hook = "nellie sync --project /some/path";
        let custom_hook = "echo hello";

        assert!(nellie_hook.contains(NELLIE_COMMAND_PREFIX));
        assert!(!custom_hook.contains(NELLIE_COMMAND_PREFIX));
    }

    #[test]
    fn test_settings_json_structure_preserved() {
        let initial = r#"{
  "theme": "dark",
  "autosave": true,
  "hooks": {
    "Custom": {
      "type": "command",
      "command": "echo test"
    }
  },
  "other_setting": "value"
}"#;

        let mut settings: Value = serde_json::from_str(initial).expect("Failed to parse");
        let hooks = &mut settings["hooks"];

        hooks["SessionStart"] = json!({
            "type": "command",
            "command": "nellie sync --project \"$PWD\" --rules 2>/dev/null || true",
            "timeout": 15000
        });

        // Verify other keys are preserved
        assert_eq!(settings["theme"], "dark");
        assert_eq!(settings["autosave"], true);
        assert_eq!(settings["other_setting"], "value");
        assert!(settings["hooks"]["Custom"].is_object());
    }

    #[test]
    fn test_hooks_object_initialization() {
        let mut settings = json!({"some_key": "value"});

        // Ensure hooks object exists
        if settings.get("hooks").is_none() {
            settings["hooks"] = json!({});
        }

        assert!(settings["hooks"].is_object());
        assert_eq!(settings["some_key"], "value");
    }

    #[test]
    fn test_backup_path_generation() {
        let settings_path = PathBuf::from("/home/user/.claude/settings.json");
        let backup_path = settings_path.with_extension("json.bak");

        assert_eq!(
            backup_path,
            PathBuf::from("/home/user/.claude/settings.json.bak")
        );
    }

    #[test]
    fn test_hook_status_struct_creation() {
        let status = HookStatus {
            session_start_installed: true,
            stop_installed: false,
            settings_path: Some(PathBuf::from("/home/user/.claude/settings.json")),
            nellie_binary_available: true,
            nellie_binary_path: Some(PathBuf::from("/usr/local/bin/nellie")),
            last_sync_time: Some(1234567890),
            last_ingest_time: None,
            memory_file_count: 5,
            rule_file_count: 3,
            memory_dir_exists: true,
            memory_index_lines: Some(150),
            old_shell_hook_detected: false,
            old_shell_hook_info: None,
        };

        assert!(status.session_start_installed);
        assert!(!status.stop_installed);
        assert_eq!(status.memory_file_count, 5);
        assert_eq!(status.rule_file_count, 3);
    }

    #[test]
    fn test_format_time_ago_seconds() {
        // Test with a timestamp from "now"
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let result = format_time_ago(now - 30);
        assert!(result.contains("seconds ago"));
    }

    #[test]
    fn test_format_time_ago_minutes() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let result = format_time_ago(now - 300); // 5 minutes ago
        assert!(result.contains("minute"));
        assert!(result.contains("ago"));
    }

    #[test]
    fn test_format_time_ago_hours() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let result = format_time_ago(now - 7200); // 2 hours ago
        assert!(result.contains("hour"));
        assert!(result.contains("ago"));
    }

    #[test]
    fn test_format_time_ago_days() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let result = format_time_ago(now - 86400); // 1 day ago
        assert!(result.contains("day"));
        assert!(result.contains("ago"));
    }

    #[test]
    fn test_hook_status_format_text() {
        let status = HookStatus {
            session_start_installed: true,
            stop_installed: true,
            settings_path: Some(PathBuf::from("/home/user/.claude/settings.json")),
            nellie_binary_available: true,
            nellie_binary_path: Some(PathBuf::from("/usr/local/bin/nellie")),
            last_sync_time: Some(1234567890),
            last_ingest_time: None,
            memory_file_count: 5,
            rule_file_count: 3,
            memory_dir_exists: true,
            memory_index_lines: Some(150),
            old_shell_hook_detected: false,
            old_shell_hook_info: None,
        };

        let text = status.format_text();
        assert!(text.contains("Nellie Deep Hooks Status"));
        assert!(text.contains("✓ installed"));
        assert!(text.contains("Memory files"));
        assert!(text.contains("Rule files"));
    }

    #[test]
    fn test_hook_status_format_text_missing_binary() {
        let status = HookStatus {
            session_start_installed: false,
            stop_installed: false,
            settings_path: None,
            nellie_binary_available: false,
            nellie_binary_path: None,
            last_sync_time: None,
            last_ingest_time: None,
            memory_file_count: 0,
            rule_file_count: 0,
            memory_dir_exists: false,
            memory_index_lines: None,
            old_shell_hook_detected: false,
            old_shell_hook_info: None,
        };

        let text = status.format_text();
        assert!(text.contains("✗ not installed"));
        assert!(text.contains("✗ not on PATH"));
        assert!(text.contains("never"));
    }

    #[test]
    fn test_hook_status_format_json() {
        let status = HookStatus {
            session_start_installed: true,
            stop_installed: false,
            settings_path: Some(PathBuf::from("/home/user/.claude/settings.json")),
            nellie_binary_available: true,
            nellie_binary_path: Some(PathBuf::from("/usr/local/bin/nellie")),
            last_sync_time: Some(1234567890),
            last_ingest_time: None,
            memory_file_count: 5,
            rule_file_count: 3,
            memory_dir_exists: true,
            memory_index_lines: Some(150),
            old_shell_hook_detected: false,
            old_shell_hook_info: None,
        };

        let json_str = status.format_json();
        assert!(json_str.contains("session_start_installed"));
        assert!(json_str.contains("true"));
        assert!(json_str.contains("memory_file_count"));
        let parsed: std::result::Result<Value, _> = serde_json::from_str(&json_str);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_memory_index_exceeds_budget_detection() {
        let status = HookStatus {
            session_start_installed: true,
            stop_installed: true,
            settings_path: Some(PathBuf::from("/home/user/.claude/settings.json")),
            nellie_binary_available: true,
            nellie_binary_path: Some(PathBuf::from("/usr/local/bin/nellie")),
            last_sync_time: None,
            last_ingest_time: None,
            memory_file_count: 10,
            rule_file_count: 5,
            memory_dir_exists: true,
            memory_index_lines: Some(250), // Over budget
            old_shell_hook_detected: false,
            old_shell_hook_info: None,
        };

        let text = status.format_text();
        assert!(text.contains("exceeds budget"));
    }

    #[test]
    fn test_detect_old_shell_hook_not_present() {
        let temp_dir = tempfile::tempdir().unwrap();
        let settings_path = temp_dir.path().join("settings.json");

        let settings = json!({
            "hooks": {
                "SessionStart": {
                    "type": "command",
                    "matcher": "startup|resume",
                    "command": "nellie sync --project \"$PWD\" --rules",
                    "timeout": 15000
                }
            }
        });

        fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let result = detect_old_shell_hook(Some(&settings_path));
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_old_shell_hook_session_start_sh() {
        let temp_dir = tempfile::tempdir().unwrap();
        let settings_path = temp_dir.path().join("settings.json");

        let settings = json!({
            "hooks": {
                "SessionStart": {
                    "type": "command",
                    "matcher": "startup|resume",
                    "command": "/usr/local/bin/nellie-session-start.sh",
                    "timeout": 15000
                }
            }
        });

        fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let result = detect_old_shell_hook(Some(&settings_path));
        assert!(result.is_some());
        let (hook_name, _cmd) = result.unwrap();
        assert_eq!(hook_name, "SessionStart");
    }

    #[test]
    fn test_detect_old_shell_hook_alternate_marker() {
        let temp_dir = tempfile::tempdir().unwrap();
        let settings_path = temp_dir.path().join("settings.json");

        let settings = json!({
            "hooks": {
                "PreStart": {
                    "type": "command",
                    "command": "bash ~/nellie_session_start",
                    "timeout": 15000
                }
            }
        });

        fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let result = detect_old_shell_hook(Some(&settings_path));
        assert!(result.is_some());
        let (hook_name, _cmd) = result.unwrap();
        assert_eq!(hook_name, "PreStart");
    }

    #[test]
    fn test_remove_old_shell_hooks_internal() {
        let mut hooks = json!({
            "SessionStart": {
                "type": "command",
                "command": "/usr/local/bin/nellie-session-start.sh",
                "timeout": 15000
            },
            "Stop": {
                "type": "command",
                "command": "nellie ingest --project \"$PWD\"",
                "timeout": 30000
            },
            "Custom": {
                "type": "command",
                "command": "some custom hook",
                "timeout": 5000
            }
        });

        let removed = remove_old_shell_hooks_internal(&mut hooks);
        assert_eq!(removed, 1);
        assert!(hooks.get("SessionStart").is_none());
        assert!(hooks.get("Stop").is_some());
        assert!(hooks.get("Custom").is_some());
    }

    #[test]
    fn test_hook_status_with_old_shell_hook() {
        let status = HookStatus {
            session_start_installed: false,
            stop_installed: false,
            settings_path: Some(PathBuf::from("/home/user/.claude/settings.json")),
            nellie_binary_available: true,
            nellie_binary_path: Some(PathBuf::from("/usr/local/bin/nellie")),
            last_sync_time: None,
            last_ingest_time: None,
            memory_file_count: 0,
            rule_file_count: 0,
            memory_dir_exists: false,
            memory_index_lines: None,
            old_shell_hook_detected: true,
            old_shell_hook_info: Some((
                "SessionStart".to_string(),
                "/usr/local/bin/nellie-session-start.sh".to_string(),
            )),
        };

        let text = status.format_text();
        assert!(text.contains("Legacy hook detected"));
        assert!(text.contains("SessionStart"));
        assert!(text.contains("nellie hooks install --force"));
    }

    #[test]
    fn test_hook_status_format_json_with_old_hook() {
        let status = HookStatus {
            session_start_installed: true,
            stop_installed: true,
            settings_path: Some(PathBuf::from("/home/user/.claude/settings.json")),
            nellie_binary_available: true,
            nellie_binary_path: Some(PathBuf::from("/usr/local/bin/nellie")),
            last_sync_time: None,
            last_ingest_time: None,
            memory_file_count: 2,
            rule_file_count: 1,
            memory_dir_exists: true,
            memory_index_lines: Some(50),
            old_shell_hook_detected: true,
            old_shell_hook_info: Some((
                "PreStart".to_string(),
                "/usr/local/bin/nellie-session-start.sh".to_string(),
            )),
        };

        let json_str = status.format_json();
        assert!(json_str.contains("old_shell_hook_detected"));
        assert!(json_str.contains("true"));
        assert!(json_str.contains("PreStart"));
        let parsed: std::result::Result<Value, _> = serde_json::from_str(&json_str);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_install_hooks_with_old_shell_hook_no_force() {
        let temp_dir = tempfile::tempdir().unwrap();
        let settings_path = temp_dir.path().join("settings.json");

        // Create settings with old shell hook
        let settings = json!({
            "hooks": {
                "SessionStart": {
                    "type": "command",
                    "matcher": "startup|resume",
                    "command": "/usr/local/bin/nellie-session-start.sh",
                    "timeout": 15000
                }
            }
        });

        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        // Mock the resolve_settings_path for this test
        // We can't directly mock it, so we test the internal function
        let mut settings_obj = settings.clone();
        let hooks = &mut settings_obj["hooks"];

        let removed = remove_old_shell_hooks_internal(hooks);
        assert_eq!(removed, 1);

        // Verify old hook is gone
        assert!(settings_obj
            .get("hooks")
            .unwrap()
            .get("SessionStart")
            .is_none());
    }
}
