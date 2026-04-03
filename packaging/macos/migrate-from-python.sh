#!/bin/bash
# Migration script: Python Nellie -> Nellie-RS
# Copies lessons and checkpoints from Python Nellie's ChromaDB to Nellie-RS SQLite

set -e

# Configuration
PYTHON_NELLIE_HOST="${PYTHON_NELLIE_HOST:-localhost}"
PYTHON_NELLIE_PORT="${PYTHON_NELLIE_PORT:-8765}"
RUST_NELLIE_HOST="${RUST_NELLIE_HOST:-localhost}"
RUST_NELLIE_PORT="${RUST_NELLIE_PORT:-8767}"

PYTHON_URL="http://${PYTHON_NELLIE_HOST}:${PYTHON_NELLIE_PORT}"
RUST_URL="http://${RUST_NELLIE_HOST}:${RUST_NELLIE_PORT}"

echo "=== Nellie Migration: Python -> Rust ==="
echo ""
echo "Source (Python): $PYTHON_URL"
echo "Target (Rust):   $RUST_URL"
echo ""

# Check both services are running
echo "Checking services..."

if ! curl -s "$PYTHON_URL/health" > /dev/null 2>&1; then
    echo "Error: Python Nellie not responding at $PYTHON_URL/health"
    echo "Make sure Python Nellie is running on port $PYTHON_NELLIE_PORT"
    exit 1
fi
echo "  Python Nellie: OK"

if ! curl -s "$RUST_URL/health" > /dev/null 2>&1; then
    echo "Error: Nellie-RS not responding at $RUST_URL/health"
    echo "Make sure Nellie-RS is running on port $RUST_NELLIE_PORT"
    exit 1
fi
echo "  Nellie-RS: OK"
echo ""

# Create temp directory for migration data
TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

echo "=== Step 1: Export Lessons from Python Nellie ==="

# Use MCP to list all lessons from Python Nellie
# Python Nellie exposes search_lessons tool
curl -s -X POST "$PYTHON_URL/mcp/invoke" \
    -H "Content-Type: application/json" \
    -d '{"name": "search_lessons", "arguments": {"query": "", "limit": 10000}}' \
    > "$TMPDIR/lessons.json"

LESSON_COUNT=$(jq '.content | length' "$TMPDIR/lessons.json" 2>/dev/null || echo "0")
echo "  Found $LESSON_COUNT lessons to migrate"

if [[ "$LESSON_COUNT" == "0" || "$LESSON_COUNT" == "null" ]]; then
    echo "  No lessons found or error reading lessons."
    echo "  Skipping lessons migration."
else
    echo ""
    echo "=== Step 2: Import Lessons to Nellie-RS ==="

    # Extract lessons and import each one
    IMPORTED=0
    FAILED=0

    jq -c '.content[]' "$TMPDIR/lessons.json" 2>/dev/null | while read -r lesson; do
        TITLE=$(echo "$lesson" | jq -r '.title // .metadata.title // "Untitled"')
        CONTENT=$(echo "$lesson" | jq -r '.content // .page_content // ""')
        TAGS=$(echo "$lesson" | jq -c '.tags // .metadata.tags // ["migrated"]')
        SEVERITY=$(echo "$lesson" | jq -r '.severity // .metadata.severity // "info"')

        # Skip if no content
        if [[ -z "$CONTENT" || "$CONTENT" == "null" ]]; then
            continue
        fi

        # Import to Rust Nellie
        RESULT=$(curl -s -X POST "$RUST_URL/mcp/invoke" \
            -H "Content-Type: application/json" \
            -d "{
                \"name\": \"add_lesson\",
                \"arguments\": {
                    \"title\": $(echo "$TITLE" | jq -Rs .),
                    \"content\": $(echo "$CONTENT" | jq -Rs .),
                    \"tags\": $TAGS,
                    \"severity\": \"$SEVERITY\"
                }
            }" 2>/dev/null)

        if echo "$RESULT" | jq -e '.error' > /dev/null 2>&1; then
            echo "  Failed: $TITLE"
            ((FAILED++)) || true
        else
            echo "  Imported: $TITLE"
            ((IMPORTED++)) || true
        fi
    done

    echo ""
    echo "  Lessons imported: $IMPORTED"
    echo "  Lessons failed: $FAILED"
fi

echo ""
echo "=== Step 3: Export Checkpoints from Python Nellie ==="

# Get recent checkpoints for all agents
curl -s -X POST "$PYTHON_URL/mcp/invoke" \
    -H "Content-Type: application/json" \
    -d '{"name": "get_recent_checkpoints", "arguments": {"agent": "*", "limit": 1000}}' \
    > "$TMPDIR/checkpoints.json" 2>/dev/null || echo '{"content": []}' > "$TMPDIR/checkpoints.json"

CHECKPOINT_COUNT=$(jq '.content | length' "$TMPDIR/checkpoints.json" 2>/dev/null || echo "0")
echo "  Found $CHECKPOINT_COUNT checkpoints to migrate"

if [[ "$CHECKPOINT_COUNT" == "0" || "$CHECKPOINT_COUNT" == "null" ]]; then
    echo "  No checkpoints found or error reading checkpoints."
    echo "  Skipping checkpoints migration."
else
    echo ""
    echo "=== Step 4: Import Checkpoints to Nellie-RS ==="

    IMPORTED=0
    FAILED=0

    jq -c '.content[]' "$TMPDIR/checkpoints.json" 2>/dev/null | while read -r checkpoint; do
        AGENT=$(echo "$checkpoint" | jq -r '.agent // "unknown"')
        WORKING_ON=$(echo "$checkpoint" | jq -r '.working_on // .state.working_on // "Migrated checkpoint"')
        STATE=$(echo "$checkpoint" | jq -c '.state // {}')

        # Import to Rust Nellie
        RESULT=$(curl -s -X POST "$RUST_URL/mcp/invoke" \
            -H "Content-Type: application/json" \
            -d "{
                \"name\": \"add_checkpoint\",
                \"arguments\": {
                    \"agent\": \"$AGENT\",
                    \"working_on\": $(echo "$WORKING_ON" | jq -Rs .),
                    \"state\": $STATE
                }
            }" 2>/dev/null)

        if echo "$RESULT" | jq -e '.error' > /dev/null 2>&1; then
            echo "  Failed: $AGENT checkpoint"
            ((FAILED++)) || true
        else
            echo "  Imported: $AGENT checkpoint"
            ((IMPORTED++)) || true
        fi
    done

    echo ""
    echo "  Checkpoints imported: $IMPORTED"
    echo "  Checkpoints failed: $FAILED"
fi

echo ""
echo "=== Step 5: Verify Migration ==="

# Get counts from Rust Nellie
RUST_STATUS=$(curl -s "$RUST_URL/api/v1/status")
RUST_LESSONS=$(echo "$RUST_STATUS" | jq '.lessons // 0')
RUST_CHECKPOINTS=$(echo "$RUST_STATUS" | jq '.checkpoints // 0')

echo "  Nellie-RS now has:"
echo "    Lessons: $RUST_LESSONS"
echo "    Checkpoints: $RUST_CHECKPOINTS"

echo ""
echo "=== Migration Complete ==="
echo ""
echo "Next steps:"
echo "  1. Verify data in Nellie-RS:"
echo "     curl '$RUST_URL/mcp/invoke' -d '{\"name\":\"search_lessons\",\"arguments\":{\"query\":\"test\",\"limit\":5}}'"
echo ""
echo "  2. Update clients to use port $RUST_NELLIE_PORT"
echo ""
echo "  3. After verification, shut down Python Nellie:"
echo "     screen -S nellie-daemon -X quit"
echo "     screen -S nellie-watchdog -X quit"
echo ""
echo "  4. Reconfigure Nellie-RS to port 8765 (edit launchd plist)"
echo ""
