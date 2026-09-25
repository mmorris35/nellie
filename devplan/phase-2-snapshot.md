# Phase 2 — Portable graph snapshot

**Goal.** Export a compressed, repo-committable snapshot of the index so a teammate
(or a fresh clone / CI job) starts warm and reindexes **only what changed since the
snapshot**, instead of a cold full index.

**Framing.** Genuinely new — there is no export/import/snapshot/compress code today
and no `zstd` dependency. But the design is straightforward because Nellie already
stores everything in one self-contained SQLite `nellie.db`, and the incremental-skip
infrastructure (`file_state` + `needs_reindex_by_metadata` + `diff_index`) already
exists to consume a snapshot.

---

## Design

A snapshot is a **project-scoped subset of the SQLite DB**, zstd-compressed, written
to a path inside the repo (default `.nellie/graph.db.zst`) that a team commits.

### What a snapshot must contain (per the schema map)
Tables required for a warm start (from `src/storage/schema.rs`, `SCHEMA_VERSION = 3`,
plus the runtime vec0 tables):
- `chunks` + `chunk_embeddings` (code + vectors — embeddings live in the same DB via
  sqlite-vec, so they travel with the snapshot; no separate vector file).
- `symbols` + `structural_edges` (structural graph).
- `graph_nodes` + `graph_edges` (semantic graph).
- `file_state` **(critical)** — carries mtime/size/hash per file so the importing
  machine can `diff_index` and skip unchanged files.
- `schema_migrations` (so the importer can verify `SCHEMA_VERSION` compatibility).

Exclude machine-local / cross-project tables from the committed artifact:
`lessons`, `checkpoints`, `agent_status`, `watch_dirs` (these are not codebase index
data and are user/agent-specific).

### Step 2.1 — Add the `zstd` dependency (exact-pinned)
- Add `zstd = "=<latest>"` to `Cargo.toml` (exact version, per house rule). Verify
  against `deny.toml` / `cargo audit`. (`flate2`/`tar` already exist but are used
  only for ONNX unpacking — do not overload them.)

### Step 2.2 — Export path
- New capability `snapshot export [--out .nellie/graph.db.zst] [--project <path>]`.
- Implementation: build the filtered snapshot DB, then zstd-compress the bytes.
  Recommended mechanism (simplest correct): use SQLite `VACUUM INTO <tmp.db>` on a
  connection that has ATTACHed a filtered view, **or** create a fresh temp SQLite DB
  and copy the required tables (see list above) scoped to the project's file paths
  (`file_state.path LIKE <project_prefix>%`, and the rows in `chunks`/`symbols`/etc.
  that reference those files). Then `zstd`-compress the temp DB to `--out`.
  - Prefer a clean copy over `VACUUM INTO` of the whole DB if the whole DB contains
    other projects or the machine-local tables above — we want a *project-scoped*,
    *index-only* artifact, not a full DB dump.
- Write atomically (temp file + rename), per the "atomic writes with rollback"
  principle in `CLAUDE.md`.
- Record a small header/manifest (schema version, nellie version, embedding dim,
  created-at, project root) — either as a `meta` table in the snapshot DB or a
  sidecar. The importer uses it to refuse incompatible snapshots loudly.

### Step 2.3 — Import path
- New capability `snapshot import [--in .nellie/graph.db.zst] [--project <path>]`.
- Decompress → open the snapshot DB → **verify** schema version + embedding dim
  match the local build; refuse with a clear error on mismatch (do not silently
  import a stale schema).
- Merge the snapshot tables into the local `nellie.db` (upsert by primary key so a
  partial local index isn't clobbered; embeddings copied as-is since dim matches).
- Immediately run the existing **`diff_index`** over the project root so any files
  changed since the snapshot get reindexed and deletions pruned — this is the
  "incremental teammate indexing" payoff, reusing `needs_reindex_by_metadata`
  (`mcp.rs:2689`) with zero new diff logic.

### Step 2.4 — Surface per house conventions
- CLI: add `snapshot` subcommand group to `main.rs` `Commands` (`export`/`import`).
- MCP: register `snapshot_export` + `snapshot_import` in `get_tools()` and dispatch
  in `invoke_tool_direct()` (`src/server/mcp.rs`), each with a typed serde input
  struct and content-array return.
- Add an integration test in `tests/` (e.g. `snapshot_integration.rs`).

### Step 2.5 — Auto-refresh hook (optional, small)
- If Phase 1 lands, optionally refresh the snapshot on a debounced timer or on
  clean reconcile so the committed artifact doesn't drift. Keep it opt-in
  (`snapshot.auto: true`, off by default) and small; skip if it balloons.

---

## Acceptance
1. **Round-trip:** index a repo → `snapshot export` → wipe local `nellie.db` →
   `snapshot import` → queries return the same symbols/edges/embeddings as before
   the wipe (structural + vector search both work off the imported data).
2. **Incremental teammate path:** export, then modify 1 file and delete 1 file →
   `snapshot import` on a clean machine → `diff_index` reindexes exactly the changed
   file and prunes the deleted one, skipping all unchanged files (assert via
   `file_state` counts / tracing).
3. **Compat guard:** importing a snapshot with a mismatched `SCHEMA_VERSION` or
   embedding dim fails with a clear, actionable error and does not corrupt the local
   DB.
4. Compression is real: `.zst` is materially smaller than the raw snapshot DB
   (sanity assert, not a fixed ratio).

## Out of scope
- Multi-project snapshots in one file, encryption, or remote snapshot hosting.
- Changing the embedding model or dim (importer only *verifies* the dim matches).
- Any change to how embeddings are generated (`src/embeddings/`).

## Definition of done
- All acceptance checks demonstrated; `fmt`/`clippy -D warnings`/`test` green.
- `zstd` exact-pinned; `deny.toml`/audit clean.
- README + `config.example.yaml` document the `snapshot` commands and the
  `.nellie/graph.db.zst` convention (and add `.nellie/` guidance for repos that
  prefer not to commit it).
