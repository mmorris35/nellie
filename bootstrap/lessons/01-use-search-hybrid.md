---
title: "Always use search_hybrid for richer results"
severity: critical
tags: ["search", "best-practice", "nellie"]
used_tools: ["nellie", "search_hybrid"]
related_concepts: ["vector search", "knowledge graph", "semantic search"]
solved_problem: "search_code returns sparse results without graph context"
---

Always use `search_hybrid` instead of `search_code` when searching Nellie. Hybrid search combines vector similarity with knowledge graph expansion, surfacing related tools, solutions, and concepts that pure vector search misses. The graph traversal adds context from previous lessons and checkpoints, giving you richer, more actionable results every time.
