# Nellie Deep Hooks System

## Overview

The Nellie Deep Hooks system replaces the legacy shell script hook (`nellie-session-start.sh`) with native Nellie CLI commands that integrate directly with Claude Code's `settings.json` hook configuration.

This system enables automatic context management across Claude Code sessions:
- **Session Start**: Syncs Nellie knowledge (lessons, checkpoints, memory files) to Claude Code memory
- **Session Stop**: Ingests completed session transcripts for passive learning

## Architecture

### Native Hooks vs Legacy Shell Hook

**Legacy Approach** (`nellie-session-start.sh`):
- Shell script manually invoked or configured in hooks
- Limited to shell environments
- Difficult to update without manual reinstallation
- No feedback on sync status

**Deep Hooks Approach**:
- Native Nellie CLI subcommands: `nellie hooks install`, `nellie hooks status`, `nellie hooks uninstall`
- Direct integration with Claude Code's `settings.json`
- Automatic installation and migration
- Real-time status checking and health monitoring

## Hook Commands

### SessionStart Hook
**Matcher**: `"startup|resume"`

Runs automatically when Claude Code starts a new session or resumes an existing one.

```bash
nellie sync --project "$PWD" --rules 2>/dev/null || true
```

**Purpose**:
- Loads Nellie lessons as conditional rules (glob-based)
- Syncs memory files to `~/.claude/projects/<project>/memory/`
- Populates critical context before the session begins

**Timeout**: 15 seconds

### Stop Hook
**Matcher**: `""` (runs at session end)

Runs automatically when Claude Code closes a session.

```bash
nellie ingest --project "$PWD" --since 1h 2>/dev/null || true
```

**Purpose**:
- Parses the completed session transcript
- Extracts learnable patterns (corrections, errors, solutions)
- Stores new lessons in Nellie database for future sessions

**Timeout**: 30 seconds

## Installation

### Automatic Installation

```bash
nellie hooks install
```

This command:
1. Detects your Claude Code installation
2. Reads `~/.claude/settings.json`
3. Adds or updates SessionStart and Stop hooks
4. Preserves all existing Claude Code hooks
5. Creates a backup at `~/.claude/settings.json.bak`

### Migration from Legacy Shell Hook

If you have an existing `nellie-session-start.sh` hook:

```bash
nellie hooks install --force
```

The `--force` flag:
- Detects the old shell hook configuration
- Reports what will be replaced
- Removes the legacy hook reference
- Installs the new native hooks
- Requires explicit confirmation (unless `--force` is specified)

## Status Checking

```bash
nellie hooks status
```

Displays:
- Installation status of both SessionStart and Stop hooks
- Whether `nellie` binary is on PATH
- Last sync time (from Nellie database)
- Last ingest time (from Nellie database)
- Memory file count and budget status
- Rule file count and health

Example output:
```
Nellie Deep Hooks Status
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

SessionStart Hook:  ✓ installed
Stop Hook:         ✓ installed
Nellie Binary:     ✓ /usr/local/bin/nellie

Last Sync:         6 hours ago
Last Ingest:       2 hours ago

Memory Files:      12 / 200 (6%)
Rules Files:       3 files

Status:            ✓ Healthy
```

For JSON output:

```bash
nellie hooks status --json
```

## Uninstallation

```bash
nellie hooks uninstall
```

Removes only the Nellie hooks from Claude Code's settings.json while preserving all other hooks you've configured.

## How It Works

### Hook Execution Flow

```
Claude Code Session Starts
    ↓
SessionStart Hook Triggered
    ↓
nellie sync --project "$PWD" --rules
    ├─ Load lessons from Nellie DB
    ├─ Convert to Claude Code memory files
    ├─ Generate conditional rules (globs)
    └─ Update MEMORY.md index
    ↓
Session Ready with Nellie Context

═══════════════════════════════════════════════

Claude Code Session Ends
    ↓
Stop Hook Triggered
    ↓
nellie ingest --project "$PWD" --since 1h
    ├─ Find recent transcripts
    ├─ Parse JSONL format
    ├─ Extract learnable patterns
    ├─ Deduplicate against existing lessons
    └─ Store new lessons in Nellie DB
    ↓
Learning Captured for Future Sessions
```

## Configuration

Hook configuration is stored in `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": {
      "type": "command",
      "matcher": "startup|resume",
      "command": "nellie sync --project \"$PWD\" --rules 2>/dev/null || true",
      "timeout": 15000
    },
    "Stop": {
      "type": "command",
      "matcher": "",
      "command": "nellie ingest --project \"$PWD\" --since 1h 2>/dev/null || true",
      "timeout": 30000
    }
  }
}
```

**Key Points**:
- Timeouts are in milliseconds (15000ms = 15s, 30000ms = 30s)
- Error redirection (`2>/dev/null || true`) prevents hook failures from interrupting Claude Code
- The `--project "$PWD"` parameter uses Claude Code's current working directory
- Matchers control when hooks run (regex-based in Claude Code)

## Troubleshooting

### Hooks Not Running

1. Check installation status:
   ```bash
   nellie hooks status
   ```

2. Verify settings.json is valid JSON:
   ```bash
   cat ~/.claude/settings.json | python -m json.tool
   ```

3. Ensure `nellie` binary is on your PATH:
   ```bash
   which nellie
   ```

### Hooks Running But Failing

1. Check hook timeout settings in `~/.claude/settings.json`
2. Run commands manually to see error output:
   ```bash
   nellie sync --project "$PWD" --rules
   nellie ingest --project "$PWD" --since 1h
   ```

3. Check Nellie database health:
   ```bash
   nellie db health
   ```

### Old Shell Hook Still Present

If you have both the old shell hook and new deep hooks:

```bash
nellie hooks install --force
```

This will detect the old hook and offer to replace it.

## Development Notes

The Deep Hooks system is implemented in `src/claude_code/hooks.rs` and provides:

- `install_hooks()`: Installs or updates hooks in settings.json
- `uninstall_hooks()`: Removes Nellie hooks while preserving others
- `check_hook_status()`: Returns detailed status information
- Old hook detection: Scans for `nellie-session-start.sh` references in settings.json

All hook operations include:
- Atomic file writes with `.tmp` + rename
- Backup creation before modification
- Preservation of existing configuration
- Comprehensive error handling

## Future Enhancements

Potential improvements for future versions:

- **Health Dashboard**: Real-time hook execution stats and logs
- **Custom Hook Templates**: User-defined hooks for project-specific automation
- **Hook Scheduling**: Run hooks on intervals, not just lifecycle events
- **Performance Monitoring**: Track sync/ingest duration and resource usage
- **Hook Chaining**: Run multiple commands in sequence as a single hook

## See Also

- [Nellie Documentation](../../README.md)
- [Claude Code Integration Guide](../claude_code/)
- [Memory Management](../../docs/memory.md)
- [Rules System](../../docs/rules.md)
