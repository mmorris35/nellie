---
title: "Use query_graph for focused knowledge graph traversal"
severity: info
tags: ["graph", "query", "knowledge-graph", "nellie"]
used_tools: ["nellie", "query_graph"]
related_concepts: ["entity types", "graph traversal", "label filtering"]
solved_problem: "finding related concepts and tools across the knowledge graph"
---

Use `query_graph` with a label for focused traversal (e.g., `query_graph {label: "rust"}`), or without a label to browse all entities. Entity types include: agent, tool, problem, solution, and concept. Graph queries reveal relationships between tools and problems that text search alone cannot surface, helping you discover patterns across your codebase.
