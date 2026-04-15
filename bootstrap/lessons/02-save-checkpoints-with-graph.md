---
title: "Include graph fields when saving checkpoints"
severity: warning
tags: ["checkpoints", "knowledge-graph", "best-practice"]
used_tools: ["nellie", "add_checkpoint"]
related_concepts: ["knowledge graph", "graph enrichment", "outcome tracking"]
solved_problem: "checkpoints without graph fields do not feed the knowledge graph"
---

When saving checkpoints with `add_checkpoint`, always include the graph fields: `solved_problem`, `used_tools`, `related_concepts`, and `outcome`. These fields feed the knowledge graph, building relationships between tools, problems, and solutions. Without them, the graph cannot learn from your work and `search_hybrid` results will be less useful over time.
