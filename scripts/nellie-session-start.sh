#!/bin/bash
# Nellie session start hook — injects graph-aware context into Claude Code sessions.
# Queries Nellie for recent checkpoints and relevant lessons, then outputs
# a context block that gets injected before the first prompt is processed.

set -e

NELLIE_HOST="${NELLIE_HOST:-localhost}"
NELLIE_PORT="${NELLIE_PORT:-8765}"
NELLIE_URL="http://${NELLIE_HOST}:${NELLIE_PORT}/mcp/invoke"

# Read the hook input (JSON with session info)
INPUT=$(cat)

# Extract project name from working directory
PROJECT_DIR=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
if [ -z "$PROJECT_DIR" ]; then
    PROJECT_DIR="$(pwd)"
fi
PROJECT_NAME=$(basename "$PROJECT_DIR")

# Quick health check (fail silently if Nellie is down)
HEALTH=$(curl -sf --connect-timeout 2 --max-time 5 "http://${NELLIE_HOST}:${NELLIE_PORT}/health" 2>/dev/null) || exit 0

# Get the most recent checkpoints (time-ordered, not semantic search)
CHECKPOINTS=$(curl -sf --connect-timeout 3 --max-time 10 "$NELLIE_URL" \
    -H "Content-Type: application/json" \
    -d "{\"name\": \"get_recent_checkpoints\", \"arguments\": {\"limit\": 3}}" 2>/dev/null) || CHECKPOINTS=""

# Search for lessons related to this project
LESSONS=$(curl -sf --connect-timeout 3 --max-time 10 "$NELLIE_URL" \
    -H "Content-Type: application/json" \
    -d "{\"name\": \"search_lessons\", \"arguments\": {\"query\": \"${PROJECT_NAME}\", \"limit\": 5}}" 2>/dev/null) || LESSONS=""

# Extract checkpoint summaries
CHECKPOINT_SUMMARY=""
if [ -n "$CHECKPOINTS" ]; then
    # get_recent_checkpoints returns {content: [...]} via HTTP invoke
    CHECKPOINT_SUMMARY=$(echo "$CHECKPOINTS" | jq -r '
        (.content // []) | if type == "array" then . else [.] end | .[0:3] | .[] |
        "- [" + .agent + "] " + .working_on
    ' 2>/dev/null) || CHECKPOINT_SUMMARY=""
fi

# Extract lesson titles
LESSON_SUMMARY=""
if [ -n "$LESSONS" ]; then
    LESSON_SUMMARY=$(echo "$LESSONS" | jq -r '
        [.content // []] | flatten | .[0:5] | .[] |
        "- [" + (.record.severity // "info") + "] " + (.record.title // "untitled")
    ' 2>/dev/null) || LESSON_SUMMARY=""
fi

# Only output if we got something useful
if [ -n "$CHECKPOINT_SUMMARY" ] || [ -n "$LESSON_SUMMARY" ]; then
    echo "<nellie-context project=\"${PROJECT_NAME}\">"
    if [ -n "$CHECKPOINT_SUMMARY" ]; then
        echo "Recent checkpoints:"
        echo "$CHECKPOINT_SUMMARY"
    fi
    if [ -n "$LESSON_SUMMARY" ]; then
        echo ""
        echo "Relevant lessons:"
        echo "$LESSON_SUMMARY"
    fi
    echo ""
    echo "Use search_hybrid (not search_code) for richer results. When saving lessons/checkpoints, include graph fields (solved_problem, used_tools, related_concepts, outcome) to feed the knowledge graph."
    echo "</nellie-context>"
fi

exit 0
