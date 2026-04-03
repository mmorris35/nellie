# Claude Code Integration Snippet

Add this block to your project's `CLAUDE.md` or your global `~/.claude/CLAUDE.md` to teach Claude Code how to use Nellie:

---

```markdown
## Nellie: Code Memory (MCP)

Nellie is a persistent memory server available via MCP tools. It provides semantic code search, lessons learned, checkpoints, and a knowledge graph.

### Session Start (ALWAYS do this)
1. Search lessons for what you're about to work on:
   `nellie.search_lessons query="<topic>"`
2. Load checkpoints for current project:
   `nellie.search_checkpoints query="<current project>"`

### During Work
- Use `search_hybrid` before asking the user how something works
- Save checkpoints after completing tasks, every 10-15 min, before risky ops
- Save lessons when you hit surprises, gotchas, or get corrected
- Include graph fields (solved_problem, used_tools, related_concepts, outcome) to feed the knowledge graph

### End of Session (MANDATORY)
Always save a final checkpoint with: summary, current state, next steps, open questions.
Include `outcome` so the graph learns what worked.

### Agent Naming
Use "username/project-name" format for the agent parameter:
- Examples: "mike/my-project", "deploy/staging"
```

---

## How to add it

**Option 1: Global (all projects)**
Add to `~/.claude/CLAUDE.md`

**Option 2: Per-project**
Add to `<project-root>/CLAUDE.md`

**Option 3: Automated**
If you use Deep Hooks (`nellie hooks-install`), the SessionStart hook automatically injects Nellie context. The CLAUDE.md snippet provides the behavioral instructions that Deep Hooks don't cover (when to save, naming conventions, mandatory end-of-session checkpoint).
