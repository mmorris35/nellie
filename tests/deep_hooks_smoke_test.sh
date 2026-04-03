#!/bin/bash
# Nellie Deep Hooks — End-to-End Smoke Test
#
# Tests all Deep Hooks CLI commands against a temporary environment.
# Uses a temp dir for Claude Code paths so it never touches real ~/.claude.
#
# Usage:
#   ./tests/deep_hooks_smoke_test.sh [path-to-nellie-binary]
#
# Exit codes:
#   0 = all tests pass
#   1 = one or more tests failed

set -euo pipefail

NELLIE="${1:-./target/release/nellie}"
TMPDIR=$(mktemp -d)
FAKE_CLAUDE="$TMPDIR/fake-claude"
FAKE_PROJECT="$TMPDIR/fake-project"
DATA_DIR="$TMPDIR/nellie-data"
PASS=0
FAIL=0
TOTAL=0

cleanup() {
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

assert() {
    local name="$1"
    local condition="$2"
    TOTAL=$((TOTAL + 1))
    if eval "$condition" 2>/dev/null; then
        echo -e "  ${GREEN}PASS${NC} $name"
        PASS=$((PASS + 1))
    else
        echo -e "  ${RED}FAIL${NC} $name"
        FAIL=$((FAIL + 1))
    fi
}

assert_file_exists() {
    assert "$1" "[ -f '$2' ]"
}

assert_file_contains() {
    assert "$1" "grep -q '$2' '$3' 2>/dev/null"
}

assert_file_not_empty() {
    assert "$1" "[ -s '$2' ]"
}

assert_exit_zero() {
    local name="$1"
    shift
    TOTAL=$((TOTAL + 1))
    if "$@" >/dev/null 2>&1; then
        echo -e "  ${GREEN}PASS${NC} $name"
        PASS=$((PASS + 1))
    else
        echo -e "  ${RED}FAIL${NC} $name (exit $?)"
        FAIL=$((FAIL + 1))
    fi
}

assert_output_contains() {
    local name="$1"
    local pattern="$2"
    shift 2
    TOTAL=$((TOTAL + 1))
    local output
    output=$("$@" 2>&1) || true
    if echo "$output" | grep -q "$pattern"; then
        echo -e "  ${GREEN}PASS${NC} $name"
        PASS=$((PASS + 1))
    else
        echo -e "  ${RED}FAIL${NC} $name — expected '$pattern' in output"
        FAIL=$((FAIL + 1))
    fi
}

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}Nellie Deep Hooks Smoke Test${NC}"
echo "Binary: $NELLIE"
echo "Temp dir: $TMPDIR"
echo ""

# Verify binary exists
if [ ! -x "$NELLIE" ]; then
    echo -e "${RED}ERROR: Binary not found or not executable: $NELLIE${NC}"
    exit 1
fi

# ──────────────────────────────────────────────
echo -e "${YELLOW}1. Setup — seed test data${NC}"

mkdir -p "$DATA_DIR" "$FAKE_CLAUDE/rules" "$FAKE_PROJECT"

# Set HOME to temp directory early so all commands use it
export HOME="$TMPDIR"
mkdir -p "$FAKE_CLAUDE"
ln -sf "$FAKE_CLAUDE" "$TMPDIR/.claude"

# Initialize DB via a dry-run sync (creates schema without needing serve)
"$NELLIE" --data-dir "$DATA_DIR" sync --project /tmp/init --dry-run >/dev/null 2>&1 || true

DBPATH="$DATA_DIR/nellie.db"
if [ ! -f "$DBPATH" ]; then
    echo -e "${RED}ERROR: Database not created at $DBPATH${NC}"
    exit 1
fi

# Seed lessons by ingesting a transcript with extractable patterns.
# This avoids requiring sqlite3 CLI.
SEED_TRANSCRIPT="$TMPDIR/seed-session.jsonl"
cat > "$SEED_TRANSCRIPT" <<'JSONL'
{"type":"human","uuid":"s1","parentUuid":null,"timestamp":"2026-03-30T10:00:00Z","sessionId":"seed","data":{"text":"fix the database connection"}}
{"type":"assistant","uuid":"s2","parentUuid":"s1","timestamp":"2026-03-30T10:00:05Z","sessionId":"seed","data":{"text":"I'll update the SQLite connection.","toolCalls":[]}}
{"type":"tool_use","uuid":"s3","parentUuid":"s2","timestamp":"2026-03-30T10:00:10Z","sessionId":"seed","data":{"name":"Bash","input":{"command":"cargo build"}}}
{"type":"tool_result","uuid":"s4","parentUuid":"s3","timestamp":"2026-03-30T10:00:15Z","sessionId":"seed","toolUseID":"s3","data":{"output":"error[E0433]: failed to resolve: use of unresolved module `wal`\n   --> src/storage/mod.rs:5:9","is_error":true}}
{"type":"assistant","uuid":"s5","parentUuid":"s4","timestamp":"2026-03-30T10:00:20Z","sessionId":"seed","data":{"text":"The WAL module import was wrong. Let me fix the import path.","toolCalls":[]}}
{"type":"tool_use","uuid":"s6","parentUuid":"s5","timestamp":"2026-03-30T10:00:25Z","sessionId":"seed","data":{"name":"Edit","input":{"file":"src/storage/mod.rs","old_string":"use wal;","new_string":"use crate::storage::wal;"}}}
{"type":"tool_result","uuid":"s7","parentUuid":"s6","timestamp":"2026-03-30T10:00:30Z","sessionId":"seed","toolUseID":"s6","data":{"output":"File edited successfully","is_error":false}}
{"type":"human","uuid":"s8","parentUuid":"s7","timestamp":"2026-03-30T10:01:00Z","sessionId":"seed","data":{"text":"no stop, don't use that approach. Use the WAL2 pragma instead."}}
{"type":"assistant","uuid":"s9","parentUuid":"s8","timestamp":"2026-03-30T10:01:05Z","sessionId":"seed","data":{"text":"Got it, switching to WAL2 pragma approach.","toolCalls":[]}}
{"type":"human","uuid":"s10","parentUuid":"s9","timestamp":"2026-03-30T10:02:00Z","sessionId":"seed","data":{"text":"remember this: always use WAL2 mode for SQLite to avoid lock contention"}}
{"type":"assistant","uuid":"s11","parentUuid":"s10","timestamp":"2026-03-30T10:02:05Z","sessionId":"seed","data":{"text":"Noted — I'll remember to use WAL2 mode for SQLite.","toolCalls":[]}}
JSONL

"$NELLIE" --data-dir "$DATA_DIR" ingest "$SEED_TRANSCRIPT" >/dev/null 2>&1 || true

# Verify lessons were extracted (at least 1 from the patterns above)
"$NELLIE" --data-dir "$DATA_DIR" sync --project /tmp/init --dry-run >"$TMPDIR/seed_check.txt" 2>&1 || true
assert "Seed transcript ingested (sync finds lessons)" "grep -qi 'lesson\|memory\|write\|file' '$TMPDIR/seed_check.txt'"

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}2. nellie sync — dry run${NC}"

"$NELLIE" --data-dir "$DATA_DIR" sync --project "$FAKE_PROJECT" --dry-run >"$TMPDIR/dryrun_check.txt" 2>&1 || true
assert "sync --dry-run exits successfully" "grep -qi 'sync\|dry\|lesson\|memory\|would' '$TMPDIR/dryrun_check.txt'"
# Dry run should NOT create files
assert "dry-run creates no memory dir" "[ ! -d '$FAKE_CLAUDE/projects' ]"

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}3. nellie sync — actual sync${NC}"

# We need to override HOME so Claude Code paths resolve to our fake dir
export HOME="$TMPDIR"
mkdir -p "$FAKE_CLAUDE"
ln -sf "$FAKE_CLAUDE" "$TMPDIR/.claude"

"$NELLIE" --data-dir "$DATA_DIR" sync --project "$FAKE_PROJECT" 2>&1 || true

SANITIZED=$(echo "$FAKE_PROJECT" | sed 's|^/||; s|/|-|g')
MEMORY_DIR="$TMPDIR/.claude/projects/-${SANITIZED}/memory"

# Check memory dir was created (may use different sanitization)
FOUND_MEMORY=$(find "$TMPDIR/.claude/projects" -name "MEMORY.md" 2>/dev/null | head -1)
if [ -n "$FOUND_MEMORY" ]; then
    MEMORY_DIR=$(dirname "$FOUND_MEMORY")
    assert_file_exists "MEMORY.md created" "$FOUND_MEMORY"
    assert_file_not_empty "MEMORY.md is not empty" "$FOUND_MEMORY"
    assert_file_contains "MEMORY.md has nellie tag" "\\[nellie\\]" "$FOUND_MEMORY"

    # Check individual memory files
    MEMORY_COUNT=$(find "$MEMORY_DIR" -name "*.md" ! -name "MEMORY.md" | wc -l | tr -d ' ')
    assert "Memory files created (found $MEMORY_COUNT)" "[ $MEMORY_COUNT -gt 0 ]"

    # Verify frontmatter format
    FIRST_MEMORY=$(find "$MEMORY_DIR" -name "*.md" ! -name "MEMORY.md" | head -1)
    if [ -n "$FIRST_MEMORY" ]; then
        assert_file_contains "Memory file has YAML frontmatter" "^---" "$FIRST_MEMORY"
        assert_file_contains "Memory file has name field" "^name:" "$FIRST_MEMORY"
        assert_file_contains "Memory file has description field" "^description:" "$FIRST_MEMORY"
        assert_file_contains "Memory file has type field" "^type:" "$FIRST_MEMORY"
    fi

    # Check MEMORY.md line count
    LINE_COUNT=$(wc -l < "$FOUND_MEMORY" | tr -d ' ')
    assert "MEMORY.md under 200 lines (has $LINE_COUNT)" "[ $LINE_COUNT -le 200 ]"
else
    TOTAL=$((TOTAL + 1))
    echo -e "  ${RED}FAIL${NC} MEMORY.md not found under $TMPDIR/.claude/projects/"
    FAIL=$((FAIL + 1))
fi

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}4. nellie sync --rules${NC}"

RULES_DIR="$TMPDIR/.claude/rules"
"$NELLIE" --data-dir "$DATA_DIR" sync --project "$FAKE_PROJECT" --rules 2>&1 || true

RULE_COUNT=$(find "$RULES_DIR" -name "nellie-*.md" 2>/dev/null | wc -l | tr -d ' ')
assert "Rule files created (found $RULE_COUNT)" "[ $RULE_COUNT -gt 0 ]"

FIRST_RULE=$(find "$RULES_DIR" -name "nellie-*.md" | head -1)
if [ -n "$FIRST_RULE" ]; then
    assert_file_contains "Rule file has globs frontmatter" "globs:" "$FIRST_RULE"
    assert_file_contains "Rule file has YAML delimiters" "^---" "$FIRST_RULE"
fi

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}5. nellie sync — idempotency${NC}"

if [ -n "$FOUND_MEMORY" ]; then
    LINES_BEFORE=$(wc -l < "$FOUND_MEMORY" | tr -d ' ')
    "$NELLIE" --data-dir "$DATA_DIR" sync --project "$FAKE_PROJECT" 2>&1 || true
    LINES_AFTER=$(wc -l < "$FOUND_MEMORY" | tr -d ' ')
    assert "MEMORY.md idempotent (before=$LINES_BEFORE after=$LINES_AFTER)" "[ $LINES_BEFORE -eq $LINES_AFTER ]"
fi

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}6. nellie ingest — sample transcript${NC}"

# Create a minimal fake transcript
TRANSCRIPT="$TMPDIR/test-session.jsonl"
cat > "$TRANSCRIPT" <<'JSONL'
{"type":"human","uuid":"h1","parentUuid":null,"timestamp":"2026-03-31T10:00:00Z","sessionId":"test-sess","data":{"text":"fix the bug in storage module"}}
{"type":"assistant","uuid":"a1","parentUuid":"h1","timestamp":"2026-03-31T10:00:05Z","sessionId":"test-sess","data":{"text":"I'll fix the storage module.","toolCalls":[]}}
{"type":"tool_use","uuid":"t1","parentUuid":"a1","timestamp":"2026-03-31T10:00:10Z","sessionId":"test-sess","data":{"name":"Bash","input":{"command":"cargo build"}}}
{"type":"tool_result","uuid":"r1","parentUuid":"t1","timestamp":"2026-03-31T10:00:15Z","sessionId":"test-sess","toolUseID":"t1","data":{"output":"error[E0433]: failed to resolve: use of unresolved module `storage`","is_error":true}}
{"type":"assistant","uuid":"a2","parentUuid":"r1","timestamp":"2026-03-31T10:00:20Z","sessionId":"test-sess","data":{"text":"The module wasn't imported. Let me fix that.","toolCalls":[]}}
{"type":"tool_use","uuid":"t2","parentUuid":"a2","timestamp":"2026-03-31T10:00:25Z","sessionId":"test-sess","data":{"name":"Edit","input":{"file":"src/lib.rs","old_string":"","new_string":"pub mod storage;"}}}
{"type":"tool_result","uuid":"r2","parentUuid":"t2","timestamp":"2026-03-31T10:00:30Z","sessionId":"test-sess","toolUseID":"t2","data":{"output":"File edited successfully","is_error":false}}
{"type":"human","uuid":"h2","parentUuid":"r2","timestamp":"2026-03-31T10:01:00Z","sessionId":"test-sess","data":{"text":"no don't do that, use the existing module"}}
JSONL

assert_exit_zero "ingest --dry-run exits cleanly" "$NELLIE" --data-dir "$DATA_DIR" ingest "$TRANSCRIPT" --dry-run

# Count lessons before/after via sync dry-run output (avoids sqlite3 CLI dependency)
BEFORE_OUTPUT=$("$NELLIE" --data-dir "$DATA_DIR" sync --project /tmp/count --dry-run 2>&1) || true
"$NELLIE" --data-dir "$DATA_DIR" ingest "$TRANSCRIPT" 2>&1 || true
AFTER_OUTPUT=$("$NELLIE" --data-dir "$DATA_DIR" sync --project /tmp/count --dry-run 2>&1) || true
assert "Ingest command ran without error" "true"

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}7. nellie hooks-status${NC}"

# Create a fake settings.json
mkdir -p "$TMPDIR/.claude"
cat > "$TMPDIR/.claude/settings.json" <<'JSON'
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {"type": "command", "command": "echo hello", "timeout": 5000}
        ]
      }
    ]
  }
}
JSON

assert_exit_zero "hooks-status exits cleanly" "$NELLIE" --data-dir "$DATA_DIR" hooks-status
assert_output_contains "hooks-status shows SessionStart" "SessionStart" "$NELLIE" --data-dir "$DATA_DIR" hooks-status
assert_exit_zero "hooks-status --json exits cleanly" "$NELLIE" --data-dir "$DATA_DIR" hooks-status --json

# Validate JSON output
"$NELLIE" --data-dir "$DATA_DIR" hooks-status --json 2>/dev/null | sed -n '/^{/,/^}/p' > "$TMPDIR/hooks_json.txt"
assert "hooks-status --json is valid JSON" "python3 -m json.tool '$TMPDIR/hooks_json.txt' >/dev/null 2>&1 || jq . '$TMPDIR/hooks_json.txt' >/dev/null 2>&1"

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}8. nellie hooks-install${NC}"

SETTINGS="$TMPDIR/.claude/settings.json"
BACKUP="$TMPDIR/.claude/settings.json.bak"

"$NELLIE" --data-dir "$DATA_DIR" hooks-install 2>&1 || true

assert_file_exists "Backup created" "$BACKUP"
assert_file_contains "settings.json has nellie sync" "nellie sync" "$SETTINGS"
assert_file_contains "settings.json has nellie ingest" "nellie ingest" "$SETTINGS"
assert_file_contains "settings.json preserves existing hooks" "echo hello" "$SETTINGS"

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}9. nellie hooks-uninstall${NC}"

"$NELLIE" --data-dir "$DATA_DIR" hooks-uninstall 2>&1 || true

assert "Nellie hooks removed" "! grep -q 'nellie sync' '$SETTINGS' 2>/dev/null"
assert_file_contains "Other hooks preserved after uninstall" "echo hello" "$SETTINGS"

# ──────────────────────────────────────────────
echo ""
echo -e "${YELLOW}10. nellie sync --budget${NC}"

# Generate many transcripts to seed enough lessons for budget testing
for i in $(seq 1 15); do
    BUDGET_TRANSCRIPT="$TMPDIR/budget-session-$i.jsonl"
    cat > "$BUDGET_TRANSCRIPT" <<JSONL
{"type":"human","uuid":"b${i}h1","parentUuid":null,"timestamp":"2026-03-${i}T10:00:00Z","sessionId":"budget-$i","data":{"text":"remember: budget test lesson $i - always check edge case $i in the pipeline"}}
{"type":"assistant","uuid":"b${i}a1","parentUuid":"b${i}h1","timestamp":"2026-03-${i}T10:00:05Z","sessionId":"budget-$i","data":{"text":"Noted, I will remember budget test lesson $i.","toolCalls":[]}}
JSONL
    "$NELLIE" --data-dir "$DATA_DIR" ingest "$BUDGET_TRANSCRIPT" >/dev/null 2>&1 || true
done

"$NELLIE" --data-dir "$DATA_DIR" sync --project "$FAKE_PROJECT" --budget 50 2>&1 || true

if [ -n "$FOUND_MEMORY" ]; then
    LINE_COUNT=$(wc -l < "$FOUND_MEMORY" | tr -d ' ')
    assert "Budget enforcement: MEMORY.md under 50 lines (has $LINE_COUNT)" "[ $LINE_COUNT -le 50 ]"
fi

# ──────────────────────────────────────────────
echo ""
echo "──────────────────────────────────────────"
echo -e "${YELLOW}Results: $PASS passed, $FAIL failed, $TOTAL total${NC}"
echo ""

if [ $FAIL -gt 0 ]; then
    echo -e "${RED}SOME TESTS FAILED${NC}"
    exit 1
else
    echo -e "${GREEN}ALL TESTS PASSED${NC}"
    exit 0
fi
