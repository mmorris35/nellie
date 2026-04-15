---
title: "Use severity levels intentionally for lessons"
severity: info
tags: ["lessons", "severity", "rules", "nellie"]
used_tools: ["nellie", "add_lesson"]
related_concepts: ["critical", "warning", "info", "conditional rules"]
solved_problem: "all lessons treated equally regardless of importance"
---

Use severity levels intentionally when creating lessons: `critical` for things that will break if ignored, `warning` for gotchas and common mistakes, `info` for tips and best practices. Critical and warning severity lessons are automatically converted to conditional rules files by `nellie sync --rules`, making them persistent guidance that Claude Code loads on every session.
