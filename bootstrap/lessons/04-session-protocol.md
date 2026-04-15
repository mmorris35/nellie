---
title: "Follow the session protocol for automatic sync and ingest"
severity: warning
tags: ["hooks", "session", "sync", "ingest", "nellie"]
used_tools: ["nellie", "hooks-install", "sync", "ingest"]
related_concepts: ["SessionStart", "SessionStop", "UserPromptSubmit", "Deep Hooks"]
solved_problem: "lessons and checkpoints are not automatically synced or ingested"
---

Follow the session protocol: `SessionStart` syncs lessons to Claude Code memory files, `SessionStop` ingests transcripts to extract new lessons, and `UserPromptSubmit` injects relevant context. Run `nellie hooks-install` to set up all three hooks automatically. This ensures your knowledge base stays current without manual intervention.
