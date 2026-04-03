# How to Use Nellie — The Complete Guide

## Overview

Nellie gives AI coding agents persistent memory across sessions. This guide covers everything you need to know: naming conventions, the session protocol, when to save lessons and checkpoints, and how to search effectively.

## Agent Naming Convention

Always use `"user/project-name"` format for the `agent` parameter:
- `"mike/sallyport"` — Mike working on SallyPort
- `"shane/billing"` — Shane working on billing
- `"deploy/staging"` — Automated deploy agent

This scopes memory per-user per-project. Different people working on the same project have separate checkpoints but shared lessons.

## Session Protocol

### Session Start

At the start of every session, search Nellie for relevant context:

```
1. Search lessons for your current topic:
   nellie.search_lessons query="<what you're about to work on>"

2. Load recent checkpoints for your project:
   nellie.search_checkpoints query="<project name>"

3. Check agent status:
   nellie.get_agent_status agent="<user/project>"
```

If Deep Hooks are installed (`nellie hooks-install`), this happens automatically via the SessionStart hook.

### During Work

- **Use `search_hybrid`** before asking the user how something works — it combines vector search with knowledge graph expansion for the richest results
- **Save checkpoints** after completing tasks, every 10-15 minutes of active work, before risky operations, and before switching context
- **Save lessons** when you hit surprising errors, find gotchas, discover better patterns, or get corrected by the user

### Session End (MANDATORY)

Always save a final checkpoint with:
- Summary of work done
- Current state (decisions, flags, key files)
- Next steps
- Open questions
- Include `outcome`: `"success"`, `"failure"`, or `"partial"`

If Deep Hooks are installed, transcript ingestion happens automatically on session stop.

## Saving Checkpoints

Use `nellie.add_checkpoint` with:

```json
{
  "agent": "mike/my-project",
  "working_on": "Brief description of current task",
  "state": {
    "decisions": ["Used async/await instead of threads", "SQLite over Postgres for MVP"],
    "flags": ["IN_PROGRESS"],
    "key_files": ["src/main.rs", "src/server.rs"],
    "next_steps": ["Add error handling", "Write integration tests"]
  },
  "tools_used": ["cargo", "git", "reqwest"],
  "problems_encountered": ["SQLite WAL lock contention"],
  "solutions_found": ["Use WAL2 mode"],
  "outcome": "partial"
}
```

The graph fields (`tools_used`, `problems_encountered`, `solutions_found`, `outcome`) feed the knowledge graph — they create typed relationships that make future searches smarter. Always include them.

## Saving Lessons

Use `nellie.add_lesson` when you learn something worth remembering:

```json
{
  "title": "Short descriptive title",
  "content": "Detailed explanation of what you learned",
  "tags": ["relevant", "tags", "for", "search"],
  "severity": "info",
  "solved_problem": "What problem this lesson addresses",
  "used_tools": ["cargo", "rustc"],
  "related_concepts": ["async", "SQLite", "WAL"]
}
```

**Severity levels:**
- `"critical"` — Must-know, will be injected as a rule in Claude Code
- `"warning"` — Important gotcha, injected as a conditional rule
- `"info"` — Good to know, available via search

**When to save:**
- You hit a surprising error and found the fix
- The user corrects your approach
- You discover a pattern that works well
- You find a gotcha that would bite someone else
- A build fails and you figure out why

## Searching

**Preferred: `search_hybrid`** — combines vector similarity with knowledge graph expansion. Returns richer results than code or lesson search alone.

```json
{
  "query": "how to handle OAuth token refresh",
  "limit": 10
}
```

**Other search tools:**
- `search_lessons` — search only lessons by text
- `search_checkpoints` — search checkpoints by text, optionally filtered by agent
- `search_code` — semantic code search across indexed repositories
- `query_graph` — traverse the knowledge graph by entity label

## Knowledge Graph

Nellie tracks relationships between entities:

**Entity types:** Tool, Problem, Solution, Concept, Agent, Project, Chunk

**Edge types:** solved, used, failed_for, related_to, depends_on, derived_from, knows, prefers

The graph gets smarter every time you provide structured metadata in checkpoints and lessons. The `tools_used`, `problems_encountered`, `solutions_found`, and `outcome` fields create edges that strengthen useful connections and weaken bad ones.

## Deep Hooks

Deep Hooks integrate Nellie directly into Claude Code's lifecycle:

```bash
# Install (one time)
nellie hooks-install --server http://your-server:8765

# What it does:
# SessionStart: syncs lessons/checkpoints into Claude Code memory files
# Stop: ingests session transcripts for new lessons automatically
```

After installation, Nellie works transparently — no manual searching or saving needed for basic operation. Critical and warning lessons become rules that are always active.

## Proactive Context Injection (v0.5.1+)

The `UserPromptSubmit` hook searches Nellie on every prompt and injects relevant context before Claude sees your message:

- Searches in <800ms
- Only injects if relevance score > 0.4
- 500-token budget (enough to be useful, not enough to bloat)
- Deduplicates against what's already in session
- Fail-open: if Nellie is slow or down, your prompt is never blocked

You don't have to remember to search. Nellie surfaces what you need automatically.

## First-Time Bootstrap

After installing Nellie for the first time:

1. **Start the server:**
   ```bash
   nellie serve --host 0.0.0.0 --port 8765 --data-dir ~/.local/share/nellie --watch ~/projects
   ```

2. **Add to Claude Code:**
   ```bash
   claude mcp add nellie --transport sse http://localhost:8765/sse --scope user
   ```

3. **Install Deep Hooks:**
   ```bash
   nellie hooks-install --server http://localhost:8765
   ```

4. **Save your first lesson** — start a Claude Code session and save something you know:
   ```
   Use the add_lesson tool to save: "Project X uses PostgreSQL 15 with pgvector for embeddings"
   ```

5. **Save your first checkpoint** — at the end of the session, save your state.

6. **Start your next session** — Nellie will pre-load your lesson. You'll never explain it again.

The knowledge compounds from here. Every session adds context. Every correction becomes a lesson. Every checkpoint preserves your state. After a week, Nellie knows your codebase better than a new team member would after a month.
