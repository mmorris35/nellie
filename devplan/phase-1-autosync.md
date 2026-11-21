# Phase 1 — Make auto-sync real

**Goal.** The index stays fresh with zero manual reindex, on a default install, and
survives git operations (branch switch, pull, rebase) that the FS watcher alone
handles poorly.

**Framing.** This is *finishing and git-ifying* the existing watcher, not building
one. The watcher (`src/watcher/`) already works and is spawned at
`src/main.rs:821-909` when watch dirs are set. Two concrete gaps remain.

---

## Gap 1 — The watcher is dormant by default

`config.example.yaml` ships `watch.paths: []`, and `src/config/settings.rs`
`Config::load()` (`:64-68`) **never parses the YAML file** — config comes only from
clap args / env. So on a normal `nellie serve`, no watch dirs → the watcher never
starts. `ANALYSIS_AND_PLAN.md` calls this the "config-file lie."

### Step 1.1 — Wire a real config-file loader
- Add a `--config <path>` flag to `serve` (`main.rs` `Commands`), plus default
  lookup at `<data_dir>/config.yaml` then `./nellie.yaml`.
- Add `serde` + `serde_yaml` (exact-pinned) and `#[derive(Deserialize)]` on the
  config structs in `src/config/settings.rs`, mirroring the existing YAML shape
  (`server`, `data`, `watch`, `graph`, `structural`, `deep_hooks`).
- Precedence, documented in one place: **CLI flag > env > config file > default.**
  Do not silently drop existing env/flag behavior — layer the file *underneath*.
- Keep `GraphConfig` (`settings.rs:263-293`) as the pattern for the new sections.

### Step 1.2 — Make `watch.paths` actually enable the watcher
- Feed parsed `watch.paths` into the same code path as `--watch`
  (`main.rs:81-82,821`). A user who sets `watch.paths` in the config file (or the
  new default lookup) gets a live watcher with no CLI flags.
- **Do not change the default to auto-watch an arbitrary directory.** Opt-in stays
  opt-in; we're only making the opt-in *reachable from config*. (If a sensible
  default is wanted — e.g. watch the data-dir's indexed roots — gate it behind an
  explicit `watch.auto: true` key, off by default.)

**Acceptance 1:** With only a `config.yaml` containing `watch: { paths: [<dir>] }`
(no CLI flags), `nellie serve` starts the watcher; editing a file under `<dir>`
reindexes just that file (confirm via `tracing` log + a follow-up query returning
the new symbol). Add an integration test alongside `tests/watcher_integration.rs`.

---

## Gap 2 — No git-HEAD awareness

The watcher reacts to individual FS events. A `git checkout`/`pull`/`merge` can
change hundreds of files at once; the debouncer coalesces bursts, but there is no
signal tied to *"the working tree just moved to a new commit"*, and any events that
land while the server is down are missed entirely (only startup reconciliation
catches those). There is **no** git-command/HEAD/hook awareness today — confirmed:
git usage is gitignore-only (`filter.rs`, `scanner.rs`).

### Step 2.1 — Watch `.git/HEAD` (and packed-refs) for commit/branch moves
- In the watcher setup, if a watched root is inside a git work tree, also watch
  `.git/HEAD` and `.git/refs/` / `.git/packed-refs` (these change on commit,
  checkout, pull, merge, rebase-finish).
- Debounce HEAD changes separately (a rebase touches HEAD many times); a single
  reconcile after the burst settles is correct.

### Step 2.2 — On HEAD change, reconcile instead of per-file churn
- Trigger the **existing** reconcile path — reuse `reconcile_with_walk`
  (`main.rs:958-994`) / the `diff_index` logic (`mcp.rs:2558-2689`,
  `needs_reindex_by_metadata`), which already diffs the working tree against
  `file_state` and indexes only changed/new files + removes deleted ones. Do **not**
  write a second diff algorithm.
- Optional optimization (only if it stays simple): scope the reconcile to the paths
  in `git diff --name-only <old_head> <new_head>` to avoid a full walk on large
  repos. If it adds meaningful complexity, skip it — the metadata-diff walk is
  already fast and is the safe default.

### Step 2.3 — Keep it dependency-light
- Prefer reading `.git/HEAD` + refs via the filesystem (already have `notify`) over
  adding a git library. Only shell out to `git` (via `std::process::Command`) for
  the optional `git diff --name-only` in 2.2, and degrade gracefully to the full
  metadata walk if `git` isn't on PATH or the dir isn't a repo.

**Acceptance 2:** With the watcher running on a git repo, `git checkout <branch>`
that changes tracked files triggers exactly one reconcile that brings the index in
line with the new tree (new files indexed, deleted files removed, unchanged files
skipped via `file_state`). Prove it in an integration test that inits a temp repo,
indexes, commits a change on a branch, checks it out, and asserts the index matches.

---

## Out of scope for Phase 1
- Replacing/rewriting the watcher, `EventHandler`, or `scanner.rs` (the `main.rs`
  path already bypasses parts of those — leave that refactor for a separate change).
- Fixing every aspect of the config-file lie beyond what the watcher needs (the full
  config-loader cleanup is a bigger item in `ANALYSIS_AND_PLAN.md`; do the minimum
  that makes `watch.paths` work cleanly, without regressing other config).

## Definition of done
- Both acceptance tests pass; `fmt`/`clippy -D warnings`/`test` green.
- `config.example.yaml` updated to reflect that `watch.paths` now works, with a
  comment on the new `--config` flag and git-HEAD behavior.
- `CHANGELOG` / README note: auto-sync is now enable-able via config, and reacts to
  git branch/commit changes.
