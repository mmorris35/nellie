---
title: "Save and resume work with checkpoints"
severity: info
tags: ["checkpoints", "resume", "workflow", "nellie"]
used_tools: ["nellie", "add_checkpoint", "get_recent_checkpoints"]
related_concepts: ["working context", "session recovery", "state persistence"]
solved_problem: "losing context when pausing and resuming work across sessions"
---

Save checkpoints with `add_checkpoint` when pausing work. Include `next_steps`, `key_files`, `decisions`, and `tools_used` in the state to capture your full working context. When resuming, `get_recent_checkpoints` picks up exactly where you left off, eliminating the need to re-read files and re-establish context from scratch.
