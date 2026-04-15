---
title: "nellie inject auto-surfaces relevant lessons on every prompt"
severity: info
tags: ["inject", "hooks", "automation", "nellie"]
used_tools: ["nellie", "inject"]
related_concepts: ["UserPromptSubmit", "hooks", "automatic context"]
solved_problem: "manually searching for relevant lessons before each prompt is tedious"
---

The `nellie inject` command auto-surfaces relevant lessons on every prompt via the `UserPromptSubmit` hook. You do not need to manually search for context -- Nellie finds matching lessons by semantic similarity and writes them to a temporary rules file that Claude Code loads automatically. Run `nellie hooks-install` to enable this behavior.
