# Nellie: Contributor Guide

Nellie is a production-grade semantic code memory system for AI coding agents. It combines persistent memory (lessons and checkpoints), semantic code search, a knowledge graph, and deep integration with Claude Code for automatic memory synchronization.

## Project Overview

Nellie provides three main capabilities:

1. **Persistent Memory**: Records lessons learned and agent checkpoints that survive across sessions and can be queried semantically
2. **Semantic Code Search**: Indexes source code across multiple repositories and enables vector-based search with graph-based expansion
3. **Claude Code Integration**: Deep Hooks system writes memories directly to Claude Code's native file system (`~/.claude/projects/`, `~/.claude/rules/`) so they load automatically at session start
4. **MCP Server**: Exposes memory, search, and indexing capabilities via the Model Context Protocol

Built in Rust with async/await (Tokio), ONNX embeddings, SQLite with sqlite-vec, knowledge graphs (petgraph), and tree-sitter-based structural code analysis.

## Project Structure

Source code lives in `src/` with the following module organization:

### `src/claude_code/` — Deep Hooks Integration
Direct integration with Claude Code's file-based memory systems. Writes memories to `.md` files with YAML frontmatter, manages conditional rules, parses session transcripts, and runs the sync/ingest pipeline.

**Key submodules**:
- `paths`: Resolves Claude Code directory paths (`~/.claude/projects/`, `~/.claude/rules/`, etc.)
- `memory_writer`: Atomic writes of memory `.md` files
- `memory_index`: Manages MEMORY.md index with line budget enforcement
- `sync`: Orchestrates lessons and checkpoints into memory files (sync command)
- `ingest`: Parses session transcripts, extracts patterns, deduplicates, and stores lessons (ingest command)
- `transcript`: JSONL transcript parser for Claude Code session files
- `extractor`: Pattern detection — finds corrections, tool failures, build failures in transcripts
- `rules`: Generates conditional rules with file glob patterns for context-aware loading
- `mappers`: Converts internal records to Claude Code memory format
- `dedup`: Semantic similarity-based deduplication to prevent memory bloat
- `daemon`: Background watcher monitoring for completed session transcripts
- `hooks`: Integrates Nellie commands into Claude Code's settings.json hooks
- `remote`: Remote server communication for systems without local Nellie

### `src/config/` — Configuration Management
Loads configuration from CLI arguments, environment variables, and `config.yaml` files. Supports both file-based config and programmatic config for embedded use.

### `src/embeddings/` — Vector Embeddings
ONNX-based sentence embedding generation. Uses a pre-trained model (configurable via config) to generate vectors for semantic search. Can operate in offline mode or delegate to a remote embedding service.

### `src/error/` — Error Types
Unified error type using `thiserror`. All fallible operations return `Result<T, NellieError>`.

### `src/graph/` — Knowledge Graph (Nellie-V)
Relationship-aware memory layer using `petgraph`. Tracks entities (tools, problems, solutions, concepts, agents, projects) and typed edges (solved, used, failed_for, related_to, depends_on, derived_from, knows, prefers). Enables graph traversal in search results and automatic relationship suggestion.

**Key submodules**:
- `entities`: Entity types (Tool, Problem, Solution, Concept, Agent, Project, Chunk)
- `memory`: In-memory graph with query capabilities
- `persistence`: SQLite serialization of graph state
- `bootstrap`: Builds graph from checkpoint/lesson records
- `matching`: Fuzzy entity matching for entity resolution
- `query`: Graph traversal and expansion logic
- `enrichment`: Automatic entity and edge creation during ingest

### `src/server/` — MCP Server & REST API
HTTP server exposing MCP (Model Context Protocol) endpoints and optional REST API. Handles tool invocation, metrics, observability, and optional web dashboard.

**Key submodules**:
- `mcp`: Tool definitions and dispatch via `invoke_tool_direct()`
- `mcp_transport`: MCP SSE transport layer
- `app`: Axum router setup and middleware
- `api`: REST endpoints for search, indexing, lessons, checkpoints
- `metrics`: Prometheus-compatible metric collection
- `sse`: Server-sent events for streaming responses
- `observability`: Tracing and structured logging
- `ui`: Optional web dashboard

### `src/storage/` — SQLite Database with Vector Search
Persistent storage using SQLite with `sqlite-vec` extension for vector similarity search. Stores code chunks, embeddings, lessons, checkpoints, agent status, and file state.

**Key submodules**:
- `connection`: SQLite connection pool and migrations
- `chunks`: Code chunk storage and embedding lookup
- `lessons`: Lesson records with metadata and search
- `checkpoints`: Agent checkpoint storage and search
- `agent_status`: Agent lifecycle tracking
- `file_state`: Incremental indexing state (mtime, hash)
- `search`: Vector similarity search across chunks, lessons, checkpoints
- `vector`: sqlite-vec integration
- `schema`: Schema definitions and migrations

### `src/structural/` — Structural Code Analysis
Tree-sitter-based extraction of function definitions, class structures, and code organization across multiple languages (Python, TypeScript, Rust, Go). Enables symbol-aware code search and semantic code chunking.

**Key submodules**:
- `parser`: Tree-sitter initialization for supported languages
- `extractor`: Symbol extraction logic
- `extractors/`: Language-specific extractors (python.rs, typescript.rs, rust_lang.rs, go.rs)
- `language`: Language detection and dispatcher
- `storage`: Persistence of extracted symbols
- `graph_builder`: Builds call graphs from structural analysis

### `src/watcher/` — File System Watching
Monitors indexed directories for file changes and triggers incremental reindexing. Respects `.gitignore` patterns and filters out build artifacts, node_modules, venv, etc.

## Building

```bash
cargo build --release
```

**Notes**:
- Minimum Rust version: 1.75 (specified in Cargo.toml)
- ONNX Runtime is downloaded automatically during first build via bundled feature
- First build with native dependencies (ONNX, sqlite-vec) takes 10-30 minutes
- Build server has ~4 cores; avoid running multiple cargo commands in parallel

## Testing & Quality

Always run checks sequentially (NEVER in parallel):

```bash
# Format check
cargo fmt --check

# Linting (errors only)
cargo clippy --workspace -- -D warnings

# Unit & integration tests
cargo test --workspace
```

Or run all at once:

```bash
cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace
```

## Coding Standards

- **Edition**: Rust 2021
- **Formatting**: `cargo fmt` (enforced in CI)
- **Linting**: `cargo clippy` with `-D warnings` (errors treated as denials)
- **Error Handling**:
  - Libraries use `thiserror` for error types
  - Binary uses `anyhow` for error handling
- **Logging**: Tracing framework via `tracing` crate. Initialize with `tracing_subscriber` in main
- **Async**: Tokio runtime for all async operations
- **Unsafe Code**: Forbidden (see `[lints.rust]` in Cargo.toml)
- **Documentation**: Public items should have doc comments; focus on "why" not "what"

## MCP Tool Registration

MCP tools are defined in `src/server/mcp.rs`:

1. **Tool Definition**: Add to the `match` statement in `invoke_tool_direct()` function (line ~919)
2. **Tool Metadata**: Register in `get_tools()` function (line ~345) with name, description, and input schema
3. **Input Validation**: Use serde to deserialize `ToolRequest::input` into a typed struct
4. **Response**: Return `ToolResponse` with content array (text, error, or image blocks)

Example:
```rust
// In invoke_tool_direct()
"search_code" => {
    let input: SearchCodeInput = serde_json::from_value(request.input)?;
    // ... implementation
    Ok(ToolResponse {
        content: vec![Content::text(result)],
        ..Default::default()
    })
}

// In get_tools()
ToolInfo {
    name: "search_code".to_string(),
    description: "Search indexed code repositories".to_string(),
    input_schema: json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"},
            "limit": {"type": "integer"}
        },
        "required": ["query"]
    }),
}
```

## Key Dependencies

- **Async Runtime**: `tokio` (full features)
- **Web Server**: `axum` with `tower` middleware
- **Database**: `rusqlite` (bundled SQLite) + `sqlite-vec` (vector search)
- **Embeddings**: `ort` (ONNX Runtime 2.0.0-rc.11 exact) + `tokenizers`
- **Parsing**: `tree-sitter` + language grammars (exact versions pinned)
- **Graph**: `petgraph` for in-memory graph structure
- **File Watching**: `notify` + `ignore` (respects .gitignore)
- **Serialization**: `serde` + `serde_json`
- **Error Handling**: `thiserror` (libraries) + `anyhow` (binary)
- **Logging**: `tracing` + `tracing-subscriber`

All versions are pinned to exact versions (no `^`, `~`, `>=` ranges) for supply chain security.

## Key Design Principles

1. **No Personal Information**: All paths, usernames, IPs are configurable or generalized. No hardcoded references to specific machines or users.
2. **Memory First**: Persistent storage (lessons, checkpoints) is the primary value. All data survives restarts and can be queried.
3. **Graph-Aware**: Relationships between tools, problems, and solutions are tracked and expanded in search results.
4. **Deep Integration**: Writes directly to Claude Code's file system so context loads automatically — MCP is secondary.
5. **Incremental Indexing**: Watches for file changes and reindexes only modified files (via file state tracking).
6. **Structured Logging**: All significant operations use `tracing` for observability.
7. **Atomic Operations**: All storage writes are atomic or include rollback logic.

## Directory Layout

```
nellie/
  src/
    claude_code/      # Claude Code integration
    config/           # Configuration
    embeddings/       # Vector embeddings
    error/            # Error types
    graph/            # Knowledge graph
    server/           # MCP server & REST API
    storage/          # SQLite + vector storage
    structural/       # Tree-sitter code analysis
    watcher/          # File system watching
    lib.rs            # Public library API
    main.rs           # CLI binary
  tests/              # Integration tests
  benches/            # Benchmarks
  examples/           # Example code
  Cargo.toml          # Manifest & dependencies
  Cargo.lock          # Locked versions
  LICENSE             # Apache 2.0
  config.example.yaml # Configuration template
  CLAUDE.md           # This file
```

## Common Tasks

### Add a new MCP tool
1. Implement logic in appropriate module (e.g., `storage::search_code()`)
2. Add tool name to `invoke_tool_direct()` match statement
3. Register in `get_tools()` with metadata and input schema
4. Add integration tests in `tests/`

### Index a new directory
1. Add path to `config.yaml` under `watch.paths`
2. Restart Nellie or call `index_repo` via MCP
3. Monitor progress via `GET /metrics` (Prometheus format)

### Debug a storage issue
- SQLite database lives at `data.dir` from config (default: `~/.local/share/nellie`)
- Use `sqlite3` CLI to inspect: `sqlite3 ~/.local/share/nellie/nellie.db`
- Schema defined in `src/storage/schema.rs`
- Migrations run automatically on first connection

### Performance profiling
- Build with `cargo build --release` (includes LTO and optimization)
- Check `GET /metrics` for:
  - `search_duration_seconds` histogram
  - `indexing_duration_seconds` histogram
  - `vector_search_count` counter
- Bench suite in `benches/` — run with `cargo bench`

## Testing Strategy

- **Unit Tests**: In-module tests using `#[test]` attributes
- **Integration Tests**: End-to-end flows in `tests/` directory
- **Fixtures**: Use `tempfile` for temporary directories in tests
- **Mocking**: Implement mock storage/embeddings as needed
- **Benchmarks**: Criterion-based benchmarks in `benches/` (use `#[bench]`)

## Release Process

1. Update version in `Cargo.toml`
2. Run full test suite: `cargo test --workspace`
3. Build release binary: `cargo build --release`
4. Create git tag: `git tag -a vX.Y.Z`
5. Push tag to repository
6. GitHub Actions builds cross-platform binaries

## Support

For issues, questions, or contributions, open an issue on GitHub or contact the maintainers.
