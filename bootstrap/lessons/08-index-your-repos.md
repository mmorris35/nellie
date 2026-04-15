---
title: "Index your code repos for search_hybrid to work"
severity: warning
tags: ["indexing", "search", "setup", "nellie"]
used_tools: ["nellie", "serve", "index"]
related_concepts: ["file watching", "code indexing", "semantic search"]
solved_problem: "search_hybrid returns no code results without indexed repositories"
---

Index your code repositories with `nellie serve --watch <dir>` for continuous indexing or `nellie index <path>` for one-time indexing. Without indexing, `search_hybrid` has no code context to search and will only return lessons and checkpoints. Indexing parses your code into semantically meaningful chunks with embeddings for accurate retrieval.
