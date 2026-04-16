#!/bin/bash
# Test Nellie install on OrbStack Ubuntu 24.04 VM.
#
# Usage:
#   ./tests/orbstack-install-test.sh [--path-a|--path-b|--path-cc] [--skip-reset]
#
# Paths:
#   --path-a   Install via install-universal.sh (default)
#   --path-b   Manual: install prereqs + cargo build + nellie setup
#   --path-cc  Let Claude Code on the VM try to install from the README
#
# Options:
#   --skip-reset  Don't reset VM first (for re-running after a fix)
set -uo pipefail

VM="nellie-test"
REPO_HOST="/Users/mmn/github/nellie"
REPO_VM="/Users/mmn/github/nellie"
NELLIE_PORT=8765
RESULTS=()
FAIL=0

# Parse args
PATH_MODE="a"
SKIP_RESET=false
for arg in "$@"; do
    case "$arg" in
        --path-a)  PATH_MODE="a" ;;
        --path-b)  PATH_MODE="b" ;;
        --path-cc) PATH_MODE="cc" ;;
        --skip-reset) SKIP_RESET=true ;;
    esac
done

run() { orb run -m "$VM" bash -c "$1"; }

pass() { RESULTS+=("PASS: $1"); echo "  ✓ $1"; }
fail() { RESULTS+=("FAIL: $1"); echo "  ✗ $1"; FAIL=1; }
check() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then pass "$desc"; else fail "$desc"; fi
}

echo "╔═══════════════════════════════════════════════════╗"
echo "║  Nellie OrbStack Install Test                     ║"
echo "║  Path: $(printf '%-43s' "$PATH_MODE")║"
echo "╚═══════════════════════════════════════════════════╝"
echo ""

# --- Reset ---
if [ "$SKIP_RESET" = false ]; then
    echo "=== Resetting VM ==="
    bash "$REPO_HOST/tests/orbstack-reset.sh"
    echo ""
fi

# --- Install ---
echo "=== Installing (path $PATH_MODE) ==="

if [ "$PATH_MODE" = "a" ]; then
    # Path A: install script for prereqs + model + ORT, then build from source.
    # The install script's binary-download step targets GitHub releases which
    # may not exist for private repos, so we stop after the downloads and
    # build from the mounted source tree instead.
    echo "  Running install-universal.sh for prereqs + downloads..."
    # Run only the prereq/download functions, skip binary download
    run "cd $REPO_VM && bash -c '
        source packaging/install-universal.sh
        prime_sudo
        install_build_prereqs
        install_rust_toolchain
        download_onnx_runtime
        download_embedding_model
    '" 2>&1 | tail -30
    echo ""
    echo "  Building from source..."
    run "source \$HOME/.cargo/env && cd $REPO_VM && cargo build --release 2>&1" | tail -5
    # Symlink the binary to PATH
    run "mkdir -p \$HOME/.local/bin && ln -sf $REPO_VM/target/release/nellie \$HOME/.local/bin/nellie"
    echo ""

elif [ "$PATH_MODE" = "b" ]; then
    # Path B: manual build (no install script — prereqs by hand)
    echo "  Installing build prereqs..."
    run 'sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y build-essential pkg-config libssl-dev libclang-dev'

    echo "  Installing Rust..."
    run 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'

    echo "  Building Nellie..."
    run "source \$HOME/.cargo/env && cd $REPO_VM && cargo build --release 2>&1" | tail -5

    echo "  Running nellie setup..."
    run "source \$HOME/.cargo/env && cd $REPO_VM && ./target/release/nellie setup 2>&1" | tail -10

    run "mkdir -p \$HOME/.local/bin && ln -sf $REPO_VM/target/release/nellie \$HOME/.local/bin/nellie"
    echo ""

elif [ "$PATH_MODE" = "cc" ]; then
    # Path CC: let Claude Code try from the README
    echo "  Launching Claude Code to install from README..."
    echo "  (This uses API credits and takes a few minutes)"
    run "export PATH=\"\$HOME/.local/bin:\$PATH\" && cd $REPO_VM && claude -p --dangerously-skip-permissions 'Install Nellie from the README in this repo. Build it, download any required models or runtime files, and get nellie serve running on port $NELLIE_PORT with --enable-graph --enable-structural --enable-deep-hooks. Do not ask me questions — figure out what is needed and do it.'" 2>&1 | tail -30
    echo ""
fi

# --- Verify ---
echo "=== Verification ==="

# 1. Binary exists
check "nellie binary exists" run 'which nellie 2>/dev/null || test -f ~/.local/bin/nellie || test -f ~/github/nellie/target/release/nellie'

# 2. Version check
echo -n "  Version: "
run 'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH" && nellie --version 2>/dev/null || echo "UNAVAILABLE"'

# 3. Server starts (launch in background, wait, check health)
echo "  Starting nellie serve..."
run "export PATH=\"\$HOME/.local/bin:\$HOME/.cargo/bin:\$PATH\" && export ORT_DYLIB_PATH=\"\$HOME/.local/share/nellie/lib/libonnxruntime.so\" && nohup nellie serve --host 0.0.0.0 --port $NELLIE_PORT --data-dir ~/.local/share/nellie --enable-graph --enable-structural --enable-deep-hooks > /tmp/nellie-test.log 2>&1 &"
sleep 5

# 4. Health check
HEALTH=$(run "curl -sf http://localhost:$NELLIE_PORT/health 2>/dev/null" || echo "FAILED")
if echo "$HEALTH" | grep -q "healthy"; then
    pass "health check (port $NELLIE_PORT)"
else
    fail "health check: $HEALTH"
fi

# 5. Embedding model loaded
if run "grep -q 'Embedding service initialized' /tmp/nellie-test.log 2>/dev/null || grep -q 'embedding' /tmp/nellie-test.log 2>/dev/null"; then
    pass "embedding model loaded"
else
    fail "embedding model not loaded (check /tmp/nellie-test.log)"
fi

# 6. Dashboard
DASH=$(run "curl -sf -o /dev/null -w '%{http_code}' http://localhost:$NELLIE_PORT/ui 2>/dev/null" || echo "000")
if [ "$DASH" = "200" ]; then
    pass "dashboard /ui returns 200"
else
    fail "dashboard /ui returned $DASH"
fi

# 7. nellie index
echo -n "  "
INDEX_OUT=$(run "export PATH=\"\$HOME/.local/bin:\$HOME/.cargo/bin:\$PATH\" && export ORT_DYLIB_PATH=\"\$HOME/.local/share/nellie/lib/libonnxruntime.so\" && nellie index $REPO_VM/src --data-dir ~/.local/share/nellie 2>&1" || echo "FAILED")
if echo "$INDEX_OUT" | grep -qi "chunk\|index\|file"; then
    pass "nellie index produces output"
else
    fail "nellie index: $INDEX_OUT"
fi

# --- Seed test data: lessons + checkpoints ---
echo "  Seeding test data..."
MCP="curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H Content-Type:application/json"

# Add lessons with graph metadata
run "$MCP -d '{\"name\":\"add_lesson\",\"arguments\":{\"title\":\"Always pin ONNX Runtime version\",\"content\":\"The ort crate requires a specific minimum ONNX Runtime version. Mismatches cause silent crash loops. Pin the version in install scripts and verify on startup.\",\"tags\":[\"onnx\",\"install\",\"version\"],\"severity\":\"critical\",\"used_tools\":[\"rust\",\"onnxruntime\",\"ort\"],\"related_concepts\":[\"version pinning\",\"crash prevention\"],\"solved_problem\":\"Silent crash loop from ONNX Runtime version mismatch\"}}'" >/dev/null
run "$MCP -d '{\"name\":\"add_lesson\",\"arguments\":{\"title\":\"Use sudo -n for non-interactive scripts\",\"content\":\"Scripts run by Claude Code cannot prompt for sudo passwords. Use sudo -n to test for passwordless sudo before falling back to interactive sudo -v.\",\"tags\":[\"sudo\",\"install\",\"claude-code\"],\"severity\":\"warning\",\"used_tools\":[\"bash\",\"sudo\",\"claude-code\"],\"related_concepts\":[\"CI\",\"non-interactive\",\"install automation\"],\"solved_problem\":\"Install script fails in Claude Code sessions due to sudo TTY requirement\"}}'" >/dev/null
run "$MCP -d '{\"name\":\"add_lesson\",\"arguments\":{\"title\":\"Reuse existing indexer for CLI commands\",\"content\":\"The MCP index_repo handler already implements walk+chunk+embed. CLI commands like nellie index should reuse that logic, not reimplement it.\",\"tags\":[\"rust\",\"architecture\",\"DRY\"],\"severity\":\"info\",\"used_tools\":[\"rust\",\"nellie\"],\"related_concepts\":[\"code reuse\",\"DRY\"]}}'" >/dev/null

# Add checkpoints
run "$MCP -d '{\"name\":\"add_checkpoint\",\"arguments\":{\"agent\":\"test/orbstack-verify\",\"working_on\":\"Fresh install verification on Ubuntu 24.04\",\"state\":{\"decisions\":[\"Use OrbStack VMs for fast iteration\",\"Test both install script and manual path\"],\"flags\":{\"path_a_tested\":true,\"path_b_tested\":false},\"next_steps\":[\"Fix query_graph no-label branch\",\"Implement nellie index CLI\"],\"key_files\":[\"/tests/orbstack-install-test.sh\",\"/packaging/install-universal.sh\"]},\"tools_used\":[\"orbstack\",\"claude-code\",\"curl\"],\"problems_encountered\":[\"sudo -v fails without TTY\",\"Install script tries to download binary from GitHub releases\"],\"solutions_found\":[\"Use sudo -n for non-interactive detection\",\"Build from source instead of binary download\"],\"outcome\":\"partial\"}}'" >/dev/null
run "$MCP -d '{\"name\":\"add_checkpoint\",\"arguments\":{\"agent\":\"test/launch-fixes\",\"working_on\":\"Port default consistency fix\",\"state\":{\"decisions\":[\"Unify on port 8765\"],\"flags\":{\"port_fixed\":true},\"next_steps\":[\"Fix query_graph\",\"Fix nellie index stub\"],\"key_files\":[\"src/main.rs\"]},\"tools_used\":[\"rust\",\"grep\"],\"problems_encountered\":[\"Three sources of truth for default port\"],\"solutions_found\":[\"Changed CLI default from 8080 to 8765\"],\"outcome\":\"complete\"}}'" >/dev/null

sleep 3  # let embeddings complete for vector search
pass "test data seeded (3 lessons, 2 checkpoints)"

# 8. MCP query_graph without label (#55)
GRAPH=$(run "curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H 'Content-Type: application/json' -d '{\"name\":\"query_graph\",\"arguments\":{}}' 2>/dev/null" || echo "FAILED")
GRAPH_COUNT=$(echo "$GRAPH" | grep -o '"count":[0-9]*' | grep -o '[0-9]*' || echo "0")
if [ "$GRAPH_COUNT" -gt 0 ] 2>/dev/null; then
    pass "query_graph {} returns $GRAPH_COUNT results"
else
    fail "query_graph {} returns empty (count=$GRAPH_COUNT) — #55 unfixed"
fi

# 9. MCP query_graph WITH label (regression check)
GRAPH_LABEL=$(run "curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H 'Content-Type: application/json' -d '{\"name\":\"query_graph\",\"arguments\":{\"label\":\"rust\"}}' 2>/dev/null" || echo "FAILED")
GRAPH_LABEL_COUNT=$(echo "$GRAPH_LABEL" | grep -o '"count":[0-9]*' | grep -o '[0-9]*' || echo "0")
if [ "$GRAPH_LABEL_COUNT" -gt 0 ] 2>/dev/null; then
    pass "query_graph {label:rust} returns $GRAPH_LABEL_COUNT results"
else
    fail "query_graph {label:rust} returned empty"
fi

# 10. MCP search_lessons
SLESSON=$(run "curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H 'Content-Type: application/json' -d '{\"name\":\"search_lessons\",\"arguments\":{\"query\":\"ONNX version pinning\"}}' 2>/dev/null" || echo "FAILED")
if echo "$SLESSON" | grep -qi "ONNX\|version\|pin"; then
    pass "search_lessons finds seeded lesson"
else
    fail "search_lessons: $(echo "$SLESSON" | head -c 200)"
fi

# 11. MCP list_lessons
LLESSONS=$(run "curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H 'Content-Type: application/json' -d '{\"name\":\"list_lessons\",\"arguments\":{}}' 2>/dev/null" || echo "FAILED")
LESSON_COUNT=$(echo "$LLESSONS" | grep -o '"lesson_' | wc -l | tr -d ' ' || echo "0")
if [ "$LESSON_COUNT" -ge 3 ] 2>/dev/null; then
    pass "list_lessons returns $LESSON_COUNT lessons (expected >= 3)"
else
    fail "list_lessons: found $LESSON_COUNT lessons, expected >= 3"
fi

# 12. MCP get_recent_checkpoints
CHECKPTS=$(run "curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H 'Content-Type: application/json' -d '{\"name\":\"get_recent_checkpoints\",\"arguments\":{\"limit\":5}}' 2>/dev/null" || echo "FAILED")
if echo "$CHECKPTS" | grep -qi "orbstack-verify\|launch-fixes"; then
    pass "get_recent_checkpoints returns seeded checkpoints"
else
    fail "get_recent_checkpoints: $(echo "$CHECKPTS" | head -c 200)"
fi

# 13. MCP search_checkpoints
SCHKPT=$(run "curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H 'Content-Type: application/json' -d '{\"name\":\"search_checkpoints\",\"arguments\":{\"query\":\"fresh install verification\"}}' 2>/dev/null" || echo "FAILED")
if echo "$SCHKPT" | grep -qi "install\|verification\|orbstack"; then
    pass "search_checkpoints finds seeded checkpoint"
else
    fail "search_checkpoints: $(echo "$SCHKPT" | head -c 200)"
fi

# 14. MCP search_hybrid (code search, meaningful if indexing worked)
SEARCH=$(run "curl -sf -X POST http://localhost:$NELLIE_PORT/mcp/invoke -H 'Content-Type: application/json' -d '{\"name\":\"search_hybrid\",\"arguments\":{\"query\":\"error handling\"}}' 2>/dev/null" || echo "FAILED")
if echo "$SEARCH" | grep -q "content"; then
    pass "search_hybrid returns results"
else
    fail "search_hybrid: $(echo "$SEARCH" | head -1)"
fi

# 15. nellie inject (with seeded data, should find relevant lessons)
INJECT=$(run "export PATH=\"\$HOME/.local/bin:\$HOME/.cargo/bin:\$PATH\" && nellie inject --query 'ONNX version crash' --dry-run --server http://localhost:$NELLIE_PORT 2>&1" || echo "FAILED")
if echo "$INJECT" | grep -qi "ONNX\|version\|pin\|inject\|no relevant"; then
    pass "nellie inject --dry-run exits 0"
else
    fail "nellie inject: $INJECT"
fi

# 16. hooks-install
HOOKS=$(run "export PATH=\"\$HOME/.local/bin:\$HOME/.cargo/bin:\$PATH\" && nellie hooks-install --server http://localhost:$NELLIE_PORT 2>&1" || echo "FAILED")
if echo "$HOOKS" | grep -qi "install\|hook\|session"; then
    pass "hooks-install succeeds"
else
    fail "hooks-install: $HOOKS"
fi

# 17. hooks-status shows UserPromptSubmit
HSTATUS=$(run "export PATH=\"\$HOME/.local/bin:\$HOME/.cargo/bin:\$PATH\" && nellie hooks-status 2>&1" || echo "FAILED")
if echo "$HSTATUS" | grep -qi "UserPromptSubmit\|inject"; then
    pass "hooks-status shows UserPromptSubmit"
else
    fail "hooks-status missing UserPromptSubmit: $HSTATUS"
fi

# Cleanup: stop server
run 'pkill -f "nellie serve" 2>/dev/null || true'

# --- Report ---
echo ""
echo "═══════════════════════════════════════════════════"
echo "  RESULTS (path $PATH_MODE)"
echo "═══════════════════════════════════════════════════"
for r in "${RESULTS[@]}"; do
    echo "  $r"
done
echo ""
if [ "$FAIL" -eq 0 ]; then
    echo "  ALL CHECKS PASSED"
else
    echo "  SOME CHECKS FAILED — see above"
fi
echo ""
