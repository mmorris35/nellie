# Devplan: Auto-Sync + Portable Graph Snapshot

Two enhancements borrowed from `DeusData/codebase-memory-mcp` (a fully-productized
peer of Nellie's structural indexer) and adapted to Nellie's actual architecture.

> **Grounding note.** These phases were written against the code as it exists on
> `main` (v0.5.3), not from assumptions. Every "already exists" claim below is
> cited to a file. Read the citations before implementing — the biggest risk here
> is rebuilding something Nellie already has.

## The two features

1. **Auto-sync** — keep the index fresh without a manual reindex. *Mostly already
   built:* Nellie ships a working incremental file watcher (`notify` +
   `notify-debouncer-mini`, hash-gated single-file reindex) wired into
   `src/main.rs:821-909`. The gaps are (a) it's **dormant on a default install**
   because the config file is never parsed (the "config-file lie" already flagged
   in `ANALYSIS_AND_PLAN.md`), and (b) it has **no git-HEAD awareness** — it reacts
   to raw FS events, not commits / branch switches / pulls. → **Phase 1.**

2. **Portable graph snapshot** — a compressed, repo-committable snapshot so a
   teammate (or a fresh clone) starts from a warm index and only reindexes what
   changed. *Genuinely absent* (no export/snapshot/serialize/compress code, no
   `zstd` dep), but the substrate is ideal: everything lives in one self-contained
   `nellie.db` (chunks, embeddings via sqlite-vec, symbols, structural + graph
   edges, `file_state`), and the incremental-skip infra (`file_state` +
   `needs_reindex_by_metadata` + `diff_index`) already exists to consume it.
   → **Phase 2.**

## What already exists (do NOT rebuild)

| Capability | Where | Status |
|---|---|---|
| FS watcher (notify + 500ms debounce, recursive) | `src/watcher/watcher.rs` | Working |
| Watcher wired at startup (gated on watch dirs) | `src/main.rs:821-909` | Working, conditional |
| Incremental single-file reindex (blake3 hash gate) | `src/watcher/indexer.rs:44-60,215` | Working |
| gitignore-aware filtering | `src/watcher/filter.rs`, `scanner.rs` | Working |
| Incremental-skip state (mtime/size/hash) | `src/storage/file_state.rs` | Working, tested |
| Manual index tools (`index_repo`, `diff_index`, `full_reindex`, `trigger_reindex`) | `src/server/mcp.rs` | Working |
| `nellie index` CLI | `src/main.rs:118-131,1253-1388` | Working |
| Single SQLite DB holding graph+symbols+chunks+embeddings | `src/storage/`, `src/structural/storage.rs` | Working |

## What is genuinely missing (the actual work)

- Git-HEAD / branch-switch / pull awareness that triggers a reconcile (Phase 1).
- Real config-file parsing so the watcher can be enabled without CLI flags (Phase 1).
- Any export / import / snapshot / compression path; `zstd` dependency (Phase 2).

## House conventions (must honor — from `CLAUDE.md`)

- Rust 2021, MSRV 1.75. `unsafe_code = "deny"`. `clippy --workspace -- -D warnings`.
- Deps pinned to **exact** versions (no `^`/`~`/`>=`). `zstd` must be exact-pinned.
- Libraries use `thiserror`; binary uses `anyhow`. All fallible ops return `Result`.
- `tracing` for significant ops. Tokio async throughout.
- New capabilities are idiomatically **both** a CLI subcommand (`main.rs` `Commands`)
  **and** an MCP tool (register in `get_tools()` + dispatch in `invoke_tool_direct()`
  in `src/server/mcp.rs`) with a typed serde input struct and an integration test in
  `tests/`.
- No hardcoded paths / personal info — everything configurable.

## Verification gates (Nellie's real gates — NOT RED/GREEN TDD)

The repo has **no test-first mandate**; convention is inline `#[cfg(test)]` unit
tests + integration tests in `tests/`, all green. Each phase is done when, run
sequentially (never in parallel, per `CLAUDE.md`):

```
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

plus the phase's own acceptance check (below) demonstrated end-to-end.

## Phases

- [Phase 1 — Make auto-sync real](./phase-1-autosync.md)
- [Phase 2 — Portable graph snapshot](./phase-2-snapshot.md)

Phases are independent and can land as separate PRs. Phase 2 does not depend on
Phase 1.
