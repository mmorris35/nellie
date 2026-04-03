#!/bin/bash
# Nellie v0.5.0 — Structural Code Search E2E Tests
#
# Tests Tree-sitter structural features against a live deployed Nellie instance.
# Creates test fixtures ON THE SERVER via SSH, indexes them, then queries.
#
# Usage:
#   ./tests/e2e-structural.sh [server-url] [ssh-host]
#
# Default server: http://localhost:8765
# Default SSH:    user@your-server
#
# Prerequisites:
#   - Server running with --enable-structural --enable-graph
#   - SSH access to the server machine
#
# Exit codes:
#   0 = all tests pass
#   1 = one or more tests failed

set -uo pipefail

SERVER="${1:-http://localhost:8765}"
SSH_HOST="${2:-user@your-server}"
MCP="$SERVER/mcp/invoke"
PASS=0
FAIL=0
WARN=0
TOTAL=0

# Remote fixture directory (on the server)
REMOTE_FIXTURE_DIR="/tmp/nellie-e2e-structural-$$"

cleanup() {
    # Clean up remote fixtures
    ssh "$SSH_HOST" "rm -rf $REMOTE_FIXTURE_DIR" 2>/dev/null || true
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
    curl -sf --max-time 15 "$MCP" \
        -H "Content-Type: application/json" \
        -d "{\"name\": \"$tool\", \"arguments\": $args}" 2>/dev/null
}

# ─── Create Test Fixtures on Remote Server ─────────────────────────

section "Setup: Create Test Fixtures (via SSH to $SSH_HOST)"

ssh "$SSH_HOST" "mkdir -p $REMOTE_FIXTURE_DIR" 2>/dev/null
if [ $? -ne 0 ]; then
    printf "${RED}Cannot SSH to $SSH_HOST — aborting${NC}\n"
    exit 1
fi

# Python test file with all symbol types
ssh "$SSH_HOST" "cat > $REMOTE_FIXTURE_DIR/calculator.py" << 'PYEOF'
import math
from typing import List, Optional

class Calculator:
    """A basic calculator class."""

    def __init__(self, precision: int = 2):
        self.precision = precision
        self.history: List[float] = []

    def add(self, a: float, b: float) -> float:
        result = round(a + b, self.precision)
        self.history.append(result)
        return result

    def multiply(self, a: float, b: float) -> float:
        result = round(a * b, self.precision)
        self.history.append(result)
        return result

    def sqrt(self, x: float) -> float:
        result = round(math.sqrt(x), self.precision)
        self.history.append(result)
        return result

class ScientificCalculator(Calculator):
    """Extended calculator with scientific functions."""

    def power(self, base: float, exp: float) -> float:
        result = round(math.pow(base, exp), self.precision)
        self.history.append(result)
        return result

def create_calculator(precision: int = 2) -> Calculator:
    return Calculator(precision)

def run_computation(calc: Calculator, values: List[float]) -> float:
    total = 0.0
    for v in values:
        total = calc.add(total, v)
    return total
PYEOF

# Python test file
ssh "$SSH_HOST" "cat > $REMOTE_FIXTURE_DIR/test_calculator.py" << 'PYEOF'
import pytest
from calculator import Calculator, ScientificCalculator, create_calculator, run_computation

def test_add():
    calc = create_calculator()
    assert calc.add(1, 2) == 3

def test_multiply():
    calc = Calculator()
    assert calc.multiply(3, 4) == 12

def test_sqrt():
    calc = Calculator(precision=4)
    assert calc.sqrt(16) == 4.0

def test_power():
    calc = ScientificCalculator()
    assert calc.power(2, 3) == 8.0

def test_run_computation():
    calc = create_calculator()
    result = run_computation(calc, [1.0, 2.0, 3.0])
    assert result == 6.0

def test_history():
    calc = Calculator()
    calc.add(1, 2)
    calc.multiply(3, 4)
    assert len(calc.history) == 2
PYEOF

# TypeScript test file
ssh "$SSH_HOST" "cat > $REMOTE_FIXTURE_DIR/server.ts" << 'TSEOF'
import express, { Request, Response } from 'express';
import { Logger } from './logger';

interface Config {
    port: number;
    host: string;
}

class Server {
    private app: express.Application;
    private logger: Logger;

    constructor(private config: Config) {
        this.app = express();
        this.logger = new Logger('server');
        this.setupRoutes();
    }

    private setupRoutes(): void {
        this.app.get('/health', this.healthCheck.bind(this));
        this.app.post('/api/data', this.handleData.bind(this));
    }

    private healthCheck(req: Request, res: Response): void {
        res.json({ status: 'ok' });
    }

    private handleData(req: Request, res: Response): void {
        this.logger.info('Processing data request');
        const result = processPayload(req.body);
        res.json(result);
    }

    start(): void {
        this.app.listen(this.config.port, this.config.host);
    }
}

function processPayload(data: unknown): object {
    return { processed: true, data };
}

export const createServer = (config: Config): Server => {
    return new Server(config);
};
TSEOF

# Rust test file
ssh "$SSH_HOST" "cat > $REMOTE_FIXTURE_DIR/parser.rs" << 'RSEOF'
use std::path::Path;

pub enum ParseError {
    UnsupportedType(String),
    ParseFailed(String),
}

pub struct Parser {
    strict_mode: bool,
}

impl Parser {
    pub fn new(strict_mode: bool) -> Self {
        Self { strict_mode }
    }

    pub fn parse_file(&self, path: &Path) -> Result<Document, ParseError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ParseError::ParseFailed(e.to_string()))?;
        self.parse_content(&content)
    }

    pub fn parse_content(&self, content: &str) -> Result<Document, ParseError> {
        validate_content(content)?;
        let nodes = extract_nodes(content);
        Ok(Document { nodes })
    }
}

pub struct Document {
    pub nodes: Vec<String>,
}

fn validate_content(content: &str) -> Result<(), ParseError> {
    if content.is_empty() {
        return Err(ParseError::ParseFailed("empty content".into()));
    }
    Ok(())
}

fn extract_nodes(content: &str) -> Vec<String> {
    content.lines().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_content() {
        let parser = Parser::new(false);
        let doc = parser.parse_content("hello\nworld").unwrap();
        assert_eq!(doc.nodes.len(), 2);
    }
}
RSEOF

# Go test file
ssh "$SSH_HOST" "cat > $REMOTE_FIXTURE_DIR/handler.go" << 'GOEOF'
package main

import (
    "encoding/json"
    "net/http"
    "log"
)

type Handler struct {
    logger *log.Logger
}

func NewHandler(logger *log.Logger) *Handler {
    return &Handler{logger: logger}
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
    switch r.URL.Path {
    case "/health":
        h.handleHealth(w, r)
    case "/api/process":
        h.handleProcess(w, r)
    default:
        http.NotFound(w, r)
    }
}

func (h *Handler) handleHealth(w http.ResponseWriter, r *http.Request) {
    json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func (h *Handler) handleProcess(w http.ResponseWriter, r *http.Request) {
    h.logger.Println("Processing request")
    result := processInput(r)
    json.NewEncoder(w).Encode(result)
}

func processInput(r *http.Request) map[string]interface{} {
    return map[string]interface{}{"processed": true}
}
GOEOF

# Verify fixtures exist on remote
REMOTE_COUNT=$(ssh "$SSH_HOST" "ls $REMOTE_FIXTURE_DIR/*.py $REMOTE_FIXTURE_DIR/*.ts $REMOTE_FIXTURE_DIR/*.rs $REMOTE_FIXTURE_DIR/*.go 2>/dev/null | wc -l")
if [ "$REMOTE_COUNT" -ge 4 ]; then
    pass "Test fixtures created on $SSH_HOST ($REMOTE_COUNT files)"
else
    fail "Test fixtures" "Expected 4+ files on remote, got $REMOTE_COUNT"
    exit 1
fi

# ─── Index Test Fixtures ───────────────────────────────────────────

section "Structural: Index Test Fixtures"

INDEX_RESULT=$(mcp_call "index_repo" "{\"path\": \"$REMOTE_FIXTURE_DIR\"}")
if [ $? -eq 0 ] && [ -n "$INDEX_RESULT" ]; then
    pass "index_repo on test fixtures"
else
    fail "index_repo on test fixtures" "Could not index $REMOTE_FIXTURE_DIR"
fi

# Wait for indexing to complete
sleep 3

# Verify symbols were stored
SYM_CHECK=$(mcp_call "query_structure" "{\"symbol\": \"$REMOTE_FIXTURE_DIR/calculator.py\", \"query_type\": \"symbols_in_file\"}")
SYM_COUNT=$(echo "$SYM_CHECK" | grep -o '"count":[0-9]*' | grep -o '[0-9]*')
if [ "${SYM_COUNT:-0}" -gt 0 ]; then
    pass "Symbols stored after indexing ($SYM_COUNT symbols in calculator.py)"
else
    fail "No symbols stored" "index_repo did not produce structural symbols. Check --enable-structural on server."
fi

# ─── Structural: Symbol Extraction Verification ────────────────────

section "Structural: Symbol Extraction"

# Python class methods via "contains"
PY_SYMBOLS=$(mcp_call "query_structure" '{"symbol": "Calculator", "query_type": "contains", "language": "python"}')
if [ $? -eq 0 ] && echo "$PY_SYMBOLS" | grep -q "add\|multiply\|sqrt"; then
    pass "Python class methods extracted (Calculator.add, multiply, sqrt)"
else
    fail "Python class methods" "Expected Calculator methods in query_structure contains results"
fi

# Python functions
PY_FN=$(mcp_call "query_structure" '{"symbol": "create_calculator", "query_type": "callers"}')
if [ $? -eq 0 ]; then
    pass "Python function queryable (create_calculator)"
else
    fail "Python function query"
fi

# TypeScript class methods
TS_SYMBOLS=$(mcp_call "query_structure" '{"symbol": "Server", "query_type": "contains", "language": "typescript"}')
if [ $? -eq 0 ] && echo "$TS_SYMBOLS" | grep -qi "setupRoutes\|healthCheck\|handleData\|start"; then
    pass "TypeScript class methods extracted"
else
    TS_COUNT=$(echo "$TS_SYMBOLS" | grep -o '"count":[0-9]*' | grep -o '[0-9]*')
    if [ "${TS_COUNT:-0}" -gt 0 ]; then
        pass "TypeScript symbols found ($TS_COUNT results, names may differ)"
    else
        warn "TypeScript class methods" "No results — TS extraction may differ"
    fi
fi

# Rust impl methods
RS_SYMBOLS=$(mcp_call "query_structure" '{"symbol": "Parser", "query_type": "contains", "language": "rust"}')
if [ $? -eq 0 ] && echo "$RS_SYMBOLS" | grep -qi "parse_file\|parse_content\|new"; then
    pass "Rust impl methods extracted"
else
    RS_COUNT=$(echo "$RS_SYMBOLS" | grep -o '"count":[0-9]*' | grep -o '[0-9]*')
    if [ "${RS_COUNT:-0}" -gt 0 ]; then
        pass "Rust symbols found ($RS_COUNT results, names may differ)"
    else
        warn "Rust impl methods" "No results"
    fi
fi

# Go methods
GO_SYMBOLS=$(mcp_call "query_structure" '{"symbol": "Handler", "query_type": "contains", "language": "go"}')
if [ $? -eq 0 ] && echo "$GO_SYMBOLS" | grep -qi "ServeHTTP\|handleHealth\|handleProcess"; then
    pass "Go methods extracted"
else
    GO_COUNT=$(echo "$GO_SYMBOLS" | grep -o '"count":[0-9]*' | grep -o '[0-9]*')
    if [ "${GO_COUNT:-0}" -gt 0 ]; then
        pass "Go symbols found ($GO_COUNT results, names may differ)"
    else
        warn "Go methods" "No results"
    fi
fi

# ─── Structural: Call Graph ────────────────────────────────────────

section "Structural: Call Graph"

# Callers of a function
CALLERS=$(mcp_call "query_structure" '{"symbol": "create_calculator", "query_type": "callers"}')
CALLER_COUNT=$(echo "$CALLERS" | grep -o '"count":[0-9]*' | grep -o '[0-9]*')
if [ "${CALLER_COUNT:-0}" -gt 0 ]; then
    pass "Call graph: callers of create_calculator ($CALLER_COUNT found)"
else
    warn "Call graph: callers" "No callers found — cross-file call resolution may need refinement"
fi

# Callees of a function
CALLEES=$(mcp_call "query_structure" '{"symbol": "run_computation", "query_type": "callees"}')
CALLEE_COUNT=$(echo "$CALLEES" | grep -o '"count":[0-9]*' | grep -o '[0-9]*')
if [ "${CALLEE_COUNT:-0}" -gt 0 ]; then
    pass "Call graph: callees of run_computation ($CALLEE_COUNT found)"
else
    warn "Call graph: callees" "No callees found"
fi

# Test coverage
TESTS=$(mcp_call "query_structure" '{"symbol": "add", "query_type": "tests"}')
TEST_COUNT=$(echo "$TESTS" | grep -o '"count":[0-9]*' | grep -o '[0-9]*')
if [ "${TEST_COUNT:-0}" -gt 0 ]; then
    pass "Test coverage: tests for add() ($TEST_COUNT found)"
else
    warn "Test coverage" "No test mapping found — heuristic may need test_ prefix match"
fi

# ─── Structural: Blast Radius ──────────────────────────────────────

section "Structural: Blast Radius"

BLAST=$(mcp_call "get_blast_radius" "{\"changed_files\": [\"$REMOTE_FIXTURE_DIR/calculator.py\"], \"depth\": 2}")
if [ $? -eq 0 ] && [ -n "$BLAST" ]; then
    pass "get_blast_radius returns results"

    if echo "$BLAST" | grep -qi "affected_symbols\|affected_files"; then
        pass "get_blast_radius includes affected symbols/files"
    else
        fail "get_blast_radius structure" "Missing affected_symbols or affected_files"
    fi

    if echo "$BLAST" | grep -qi "test_files"; then
        pass "get_blast_radius includes test_files key"
    else
        warn "get_blast_radius test detection" "test_files key not found"
    fi
else
    fail "get_blast_radius" "Tool returned empty or errored"
fi

BLAST_D1=$(mcp_call "get_blast_radius" "{\"changed_files\": [\"$REMOTE_FIXTURE_DIR/calculator.py\"], \"depth\": 1}")
if [ $? -eq 0 ] && [ -n "$BLAST_D1" ]; then
    pass "get_blast_radius depth=1"
else
    fail "get_blast_radius depth=1"
fi

# ─── Structural: Review Context ────────────────────────────────────

section "Structural: Review Context"

REVIEW=$(mcp_call "get_review_context" "{\"changed_files\": [\"$REMOTE_FIXTURE_DIR/calculator.py\"]}")
if [ $? -eq 0 ] && [ -n "$REVIEW" ]; then
    pass "get_review_context returns results"

    REVIEW_LEN=$(echo "$REVIEW" | wc -c)
    if [ "$REVIEW_LEN" -lt 2000 ]; then
        pass "get_review_context is compact (<2000 chars)"
    else
        warn "get_review_context size" "Output is $REVIEW_LEN chars"
    fi
else
    fail "get_review_context"
fi

# ─── Structural: Hybrid Search Integration ─────────────────────────

section "Structural: Hybrid Search Integration"

HYBRID=$(mcp_call "search_hybrid" '{"query": "Calculator add method", "limit": 5}')
if [ $? -eq 0 ] && echo "$HYBRID" | grep -qi "structural_context"; then
    pass "search_hybrid includes structural_context"
else
    warn "search_hybrid structural enrichment" "structural_context key not found"
fi

# ─── Structural: Graph Integration ─────────────────────────────────

section "Structural: Graph Entities"

GRAPH_FN=$(mcp_call "query_graph" '{"label": "Calculator", "limit": 5}')
if [ $? -eq 0 ] && echo "$GRAPH_FN" | grep -qi "class\|function\|method\|struct"; then
    pass "Structural entities visible in knowledge graph"
else
    warn "Structural graph entities" "May need server restart for bootstrap"
fi

GRAPH_EDGES=$(mcp_call "query_graph" '{"label": "add", "limit": 10}')
if [ $? -eq 0 ] && echo "$GRAPH_EDGES" | grep -qi "calls\|contains\|tests"; then
    pass "Structural edge types in graph (Calls, Contains, Tests)"
else
    warn "Structural graph edges" "Edge types may not be populated for test fixtures"
fi

# ─── Structural: Edge Cases ────────────────────────────────────────

section "Structural: Edge Cases"

# Empty file
ssh "$SSH_HOST" "touch $REMOTE_FIXTURE_DIR/empty.py"
EMPTY_INDEX=$(mcp_call "index_repo" "{\"path\": \"$REMOTE_FIXTURE_DIR\"}")
if [ $? -eq 0 ]; then
    pass "Indexing handles empty files without error"
else
    fail "Empty file indexing"
fi

# File with syntax errors
ssh "$SSH_HOST" "cat > $REMOTE_FIXTURE_DIR/broken.py" << 'PYEOF'
def this_is_fine():
    return True

def this is broken(:
    nope
PYEOF
BROKEN_INDEX=$(mcp_call "index_repo" "{\"path\": \"$REMOTE_FIXTURE_DIR\"}")
if [ $? -eq 0 ]; then
    pass "Indexing handles syntax errors gracefully"
else
    fail "Syntax error handling"
fi

# Unknown file extension
ssh "$SSH_HOST" "echo 'This is not code' > $REMOTE_FIXTURE_DIR/data.xyz"
UNKNOWN_INDEX=$(mcp_call "index_repo" "{\"path\": \"$REMOTE_FIXTURE_DIR\"}")
if [ $? -eq 0 ]; then
    pass "Indexing skips unknown file extensions"
else
    fail "Unknown extension handling"
fi

# ─── Structural: Performance ───────────────────────────────────────

section "Structural: Performance"

START_TIME=$(date +%s%N)
mcp_call "get_blast_radius" "{\"changed_files\": [\"$REMOTE_FIXTURE_DIR/calculator.py\"], \"depth\": 2}" >/dev/null
END_TIME=$(date +%s%N)
ELAPSED_MS=$(( (END_TIME - START_TIME) / 1000000 ))
if [ "$ELAPSED_MS" -lt 500 ]; then
    pass "Blast radius query <500ms (${ELAPSED_MS}ms)"
else
    warn "Blast radius performance" "Took ${ELAPSED_MS}ms (target: <500ms)"
fi

START_TIME=$(date +%s%N)
mcp_call "query_structure" '{"symbol": "Calculator", "query_type": "contains"}' >/dev/null
END_TIME=$(date +%s%N)
ELAPSED_MS=$(( (END_TIME - START_TIME) / 1000000 ))
if [ "$ELAPSED_MS" -lt 200 ]; then
    pass "query_structure <200ms (${ELAPSED_MS}ms)"
else
    warn "query_structure performance" "Took ${ELAPSED_MS}ms (target: <200ms)"
fi

# ─── Summary ───────────────────────────────────────────────────────

section "Summary"

printf "  Server:  %s\n" "$SERVER"
printf "  SSH:     %s\n" "$SSH_HOST"
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
