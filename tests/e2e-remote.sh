#!/bin/bash
# Nellie v0.5.0 — End-to-End Remote Instance Test
#
# Tests all features against a live deployed Nellie instance.
# Designed for the MiniDev deployment but works with any Nellie server.
#
# Usage:
#   ./tests/e2e-remote.sh [server-url]
#
# Default server: http://localhost:8765
#
# Exit codes:
#   0 = all tests pass
#   1 = one or more tests failed

set -uo pipefail

SERVER="${1:-http://localhost:8765}"
MCP="$SERVER/mcp/invoke"
PASS=0
FAIL=0
WARN=0
TOTAL=0
TMPDIR=$(mktemp -d)
TEST_LESSON_ID=""

cleanup() {
    # Clean up test lesson if it was created
    if [ -n "$TEST_LESSON_ID" ]; then
        curl -sf --max-time 5 "$MCP" \
            -H "Content-Type: application/json" \
            -d "{\"name\": \"delete_lesson\", \"arguments\": {\"id\": \"$TEST_LESSON_ID\"}}" \
            >/dev/null 2>&1 || true
    fi
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

pass() {
    TOTAL=$((TOTAL + 1))
    PASS=$((PASS + 1))
    printf "${GREEN}  PASS${NC}  %s\n" "$1"
}

fail() {
    TOTAL=$((TOTAL + 1))
    FAIL=$((FAIL + 1))
    printf "${RED}  FAIL${NC}  %s\n" "$1"
    [ -n "${2:-}" ] && printf "        %s\n" "$2"
}

warn() {
    TOTAL=$((TOTAL + 1))
    WARN=$((WARN + 1))
    printf "${YELLOW}  WARN${NC}  %s\n" "$1"
    [ -n "${2:-}" ] && printf "        %s\n" "$2"
}

section() {
    printf "\n${CYAN}=== %s ===${NC}\n\n" "$1"
}

mcp_call() {
    local tool="$1"
    local args="$2"
    curl -sf --max-time 10 "$MCP" \
        -H "Content-Type: application/json" \
        -d "{\"name\": \"$tool\", \"arguments\": $args}" 2>/dev/null
}

# ─── Health & Basics ───────────────────────────────────────────────

section "Health & Basics"

# 1. Health check
HEALTH=$(curl -sf --max-time 5 "$SERVER/health" 2>/dev/null)
if [ $? -eq 0 ] && echo "$HEALTH" | grep -q '"healthy"'; then
    VERSION=$(echo "$HEALTH" | grep -o '"version":"[^"]*"' | cut -d'"' -f4)
    DB=$(echo "$HEALTH" | grep -o '"database":"[^"]*"' | cut -d'"' -f4)
    pass "Health check (v$VERSION, db=$DB)"
else
    fail "Health check" "Server unreachable or unhealthy at $SERVER"
    printf "\n${RED}Server is down — cannot continue.${NC}\n"
    exit 1
fi

# 2. SSE endpoint responds (streaming, so we just check it connects)
SSE_STATUS=$(curl -s --max-time 2 -o /dev/null -w "%{http_code}" "$SERVER/sse" 2>/dev/null || echo "000")
# SSE returns 200 and streams — curl exits with timeout but that's expected
if [[ "$SSE_STATUS" == 200* ]] || [ "$SSE_STATUS" = "000" ]; then
    pass "SSE endpoint responds"
else
    warn "SSE endpoint" "Got HTTP $SSE_STATUS"
fi

# 3. MCP tools list
TOOLS=$(curl -sf --max-time 5 "$SERVER/mcp/tools" 2>/dev/null)
if [ $? -eq 0 ]; then
    TOOL_COUNT=$(echo "$TOOLS" | grep -o '"name"' | wc -l)
    pass "MCP tools list ($TOOL_COUNT tools)"
else
    fail "MCP tools list"
fi

# 4. Dashboard
DASH_STATUS=$(curl -sf --max-time 5 -o /dev/null -w "%{http_code}" "$SERVER/ui" 2>/dev/null || echo "000")
if [ "$DASH_STATUS" = "200" ]; then
    pass "Dashboard /ui responds"
else
    warn "Dashboard /ui" "Got HTTP $DASH_STATUS"
fi

# ─── MCP: Status Tools ────────────────────────────────────────────

section "MCP: Status Tools"

# 5. get_status
STATUS=$(mcp_call "get_status" "{}")
if [ $? -eq 0 ] && [ -n "$STATUS" ]; then
    pass "get_status"
else
    fail "get_status"
fi

# 6. get_agent_status
AGENT_STATUS=$(mcp_call "get_agent_status" '{"agent": "e2e-test"}')
if [ $? -eq 0 ] && [ -n "$AGENT_STATUS" ]; then
    pass "get_agent_status"
else
    fail "get_agent_status"
fi

# ─── MCP: Lessons CRUD ────────────────────────────────────────────

section "MCP: Lessons CRUD"

# 7. Add a test lesson
ADD_RESULT=$(mcp_call "add_lesson" '{
    "title": "E2E Test Lesson — safe to delete",
    "content": "This is an automated test lesson created by e2e-remote.sh. If you see this, the test cleanup failed.",
    "tags": ["e2e-test", "automated"],
    "severity": "info",
    "solved_problem": "e2e test validation",
    "used_tools": ["curl", "bash"],
    "related_concepts": ["testing", "e2e"]
}')
if [ $? -eq 0 ] && echo "$ADD_RESULT" | grep -q '"id"'; then
    TEST_LESSON_ID=$(echo "$ADD_RESULT" | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)
    pass "add_lesson (id=$TEST_LESSON_ID)"
else
    fail "add_lesson"
fi

# 8. Search for the test lesson
if [ -n "$TEST_LESSON_ID" ]; then
    SEARCH_RESULT=$(mcp_call "search_lessons" '{"query": "E2E Test Lesson safe to delete", "limit": 3}')
    if [ $? -eq 0 ] && echo "$SEARCH_RESULT" | grep -q "E2E Test Lesson"; then
        pass "search_lessons (found test lesson)"
    else
        fail "search_lessons" "Test lesson not found in results"
    fi
fi

# 9. List lessons
LIST_RESULT=$(mcp_call "list_lessons" '{"limit": 5}')
if [ $? -eq 0 ] && [ -n "$LIST_RESULT" ]; then
    pass "list_lessons"
else
    fail "list_lessons"
fi

# 10. Delete test lesson
if [ -n "$TEST_LESSON_ID" ]; then
    DEL_RESULT=$(mcp_call "delete_lesson" "{\"id\": \"$TEST_LESSON_ID\"}")
    if [ $? -eq 0 ] && echo "$DEL_RESULT" | grep -q "deleted"; then
        pass "delete_lesson"
        TEST_LESSON_ID=""  # Cleared so cleanup doesn't retry
    else
        fail "delete_lesson"
    fi
fi

# ─── MCP: Checkpoints ─────────────────────────────────────────────

section "MCP: Checkpoints"

# 11. Add checkpoint
CP_RESULT=$(mcp_call "add_checkpoint" '{
    "agent": "e2e-test/remote",
    "working_on": "E2E remote test run",
    "state": {"test": true, "assertions": "in progress"},
    "tools_used": ["curl", "bash"],
    "problems_encountered": [],
    "solutions_found": [],
    "outcome": "success"
}')
if [ $? -eq 0 ] && [ -n "$CP_RESULT" ]; then
    pass "add_checkpoint"
else
    fail "add_checkpoint"
fi

# 12. Search checkpoints
CP_SEARCH=$(mcp_call "search_checkpoints" '{"query": "E2E remote test", "limit": 3}')
if [ $? -eq 0 ] && echo "$CP_SEARCH" | grep -q "e2e-test"; then
    pass "search_checkpoints (found test checkpoint)"
else
    fail "search_checkpoints"
fi

# 13. Get recent checkpoints
CP_RECENT=$(mcp_call "get_recent_checkpoints" '{"agent": "e2e-test/remote", "limit": 1}')
if [ $? -eq 0 ] && [ -n "$CP_RECENT" ]; then
    pass "get_recent_checkpoints"
else
    fail "get_recent_checkpoints"
fi

# ─── MCP: Search ──────────────────────────────────────────────────

section "MCP: Search"

# 14. search_hybrid
HYBRID=$(mcp_call "search_hybrid" '{"query": "how to use nellie", "limit": 3}')
if [ $? -eq 0 ] && [ -n "$HYBRID" ]; then
    pass "search_hybrid"
else
    fail "search_hybrid"
fi

# 15. search_code
CODE=$(mcp_call "search_code" '{"query": "authentication", "limit": 3}')
if [ $? -eq 0 ] && [ -n "$CODE" ]; then
    pass "search_code"
else
    warn "search_code" "May have no indexed repos"
fi

# ─── MCP: Knowledge Graph ─────────────────────────────────────────

section "MCP: Knowledge Graph"

# 16. query_graph
GRAPH=$(mcp_call "query_graph" '{"label": "nellie", "limit": 5}')
if [ $? -eq 0 ] && [ -n "$GRAPH" ]; then
    pass "query_graph"
else
    warn "query_graph" "Graph may be empty"
fi

# ─── MCP: Indexing ─────────────────────────────────────────────────

section "MCP: Indexing"

# 17. diff_index (safe — just checks, doesn't rebuild)
DIFF=$(mcp_call "diff_index" '{}')
if [ $? -eq 0 ] && [ -n "$DIFF" ]; then
    pass "diff_index"
else
    warn "diff_index" "May have no watch dirs configured"
fi

# ─── MCP: Structural Search ────────────────────────────────────────

section "MCP: Structural Search"

# 18. get_blast_radius
BLAST=$(mcp_call "get_blast_radius" '{"changed_files": ["src/main.rs"], "depth": 1}')
if [ $? -eq 0 ] && echo "$BLAST" | grep -q "changed_files"; then
    pass "get_blast_radius"
else
    warn "get_blast_radius" "May have no indexed structural symbols"
fi

# 19. query_structure
STRUCT=$(mcp_call "query_structure" '{"symbol": "main", "query_type": "callers"}')
if [ $? -eq 0 ] && echo "$STRUCT" | grep -q "results"; then
    pass "query_structure"
else
    warn "query_structure" "May have no indexed structural symbols"
fi

# 20. get_review_context
REVIEW=$(mcp_call "get_review_context" '{"changed_files": ["src/main.rs"]}')
if [ $? -eq 0 ] && echo "$REVIEW" | grep -q "summary"; then
    pass "get_review_context"
else
    warn "get_review_context" "May have no indexed structural symbols"
fi

# 21. search_hybrid with structural context
HYBRID_STRUCT=$(mcp_call "search_hybrid" '{"query": "function definition", "limit": 3, "structural_context": true}')
if [ $? -eq 0 ] && [ -n "$HYBRID_STRUCT" ]; then
    pass "search_hybrid with structural context"
else
    warn "search_hybrid (structural)" "May have no indexed repos or structural symbols"
fi

# ─── Deep Hooks CLI ────────────────────────────────────────────────

section "Deep Hooks CLI (via nellie binary)"

NELLIE=$(which nellie 2>/dev/null || echo "")
if [ -z "$NELLIE" ]; then
    warn "nellie binary not on PATH — skipping CLI tests"
else
    NELLIE_VER=$($NELLIE --version 2>/dev/null)
    pass "nellie binary on PATH ($NELLIE_VER)"

    # 22. sync --server --dry-run
    SYNC_OUT=$($NELLIE sync --rules --server "$SERVER" --dry-run 2>&1)
    if [ $? -eq 0 ]; then
        pass "nellie sync --server --dry-run"
    else
        fail "nellie sync --server --dry-run" "$SYNC_OUT"
    fi

    # 23. hooks-status --json
    HS_OUT=$($NELLIE hooks-status --json 2>&1)
    if [ $? -eq 0 ] && echo "$HS_OUT" | grep -q '"session_start_installed"'; then
        pass "nellie hooks-status --json"
    else
        fail "nellie hooks-status --json" "$HS_OUT"
    fi
fi

# ─── REST API Direct ──────────────────────────────────────────────

section "REST API Direct"

# 24. GET /api/v1/lessons
API_LESSONS=$(curl -sf --max-time 5 "$SERVER/api/v1/lessons?limit=3" 2>/dev/null)
if [ $? -eq 0 ] && [ -n "$API_LESSONS" ]; then
    pass "GET /api/v1/lessons"
else
    fail "GET /api/v1/lessons"
fi

# 25. GET /api/v1/search?q=...
API_SEARCH=$(curl -sf --max-time 10 "$SERVER/api/v1/search?q=nellie&limit=3" 2>/dev/null)
if [ $? -eq 0 ] && [ -n "$API_SEARCH" ]; then
    pass "GET /api/v1/search"
else
    fail "GET /api/v1/search"
fi

# 26. GET /api/v1/search/hybrid?q=...
API_HYBRID=$(curl -sf --max-time 10 "$SERVER/api/v1/search/hybrid?q=nellie&limit=3" 2>/dev/null)
if [ $? -eq 0 ] && [ -n "$API_HYBRID" ]; then
    pass "GET /api/v1/search/hybrid"
else
    fail "GET /api/v1/search/hybrid"
fi

# 27. GET /api/v1/checkpoints
API_CP=$(curl -sf --max-time 5 "$SERVER/api/v1/checkpoints?limit=3" 2>/dev/null)
if [ $? -eq 0 ] && [ -n "$API_CP" ]; then
    pass "GET /api/v1/checkpoints"
else
    fail "GET /api/v1/checkpoints"
fi

# 28. GET /api/v1/dashboard
API_DASH=$(curl -sf --max-time 5 "$SERVER/api/v1/dashboard" 2>/dev/null)
if [ $? -eq 0 ] && [ -n "$API_DASH" ]; then
    pass "GET /api/v1/dashboard"
else
    fail "GET /api/v1/dashboard"
fi

# 29. GET /api/v1/graph
API_GRAPH=$(curl -sf --max-time 5 "$SERVER/api/v1/graph?label=nellie&limit=5" 2>/dev/null)
if [ $? -eq 0 ] && [ -n "$API_GRAPH" ]; then
    pass "GET /api/v1/graph"
else
    fail "GET /api/v1/graph"
fi

# ─── Summary ───────────────────────────────────────────────────────

section "Summary"

printf "  Server:  %s\n" "$SERVER"
printf "  Total:   %d\n" "$TOTAL"
printf "  ${GREEN}Passed:  %d${NC}\n" "$PASS"
if [ "$WARN" -gt 0 ]; then
    printf "  ${YELLOW}Warned:  %d${NC}\n" "$WARN"
fi
if [ "$FAIL" -gt 0 ]; then
    printf "  ${RED}Failed:  %d${NC}\n" "$FAIL"
    printf "\n${RED}SOME TESTS FAILED${NC}\n"
    exit 1
else
    printf "\n${GREEN}ALL TESTS PASSED${NC}\n"
    exit 0
fi
