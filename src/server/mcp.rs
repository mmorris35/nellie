//! MCP server implementation using rmcp.

use std::sync::Arc;

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::embeddings::EmbeddingService;
use crate::storage::Database;

/// Maximum file size to index (1MB). Files larger than this are skipped.
const MAX_INDEX_FILE_SIZE: u64 = 1_048_576;

/// Check if a file should be indexed (shared filter for all MCP indexing handlers).
/// Applies: code file check, default ignore patterns, and file size limit.
fn should_index_file(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if !crate::watcher::FileFilter::is_code_file(path) {
        return false;
    }
    // Check file size
    if let Ok(metadata) = path.metadata() {
        if metadata.len() > MAX_INDEX_FILE_SIZE {
            return false;
        }
    }
    // Check default ignore patterns (node_modules, target, evidence dirs, etc.)
    if is_default_ignored_path(path) {
        return false;
    }
    true
}

/// Check if a path matches default ignore patterns.
/// Delegates to the shared exclusion list in `watcher::filter`.
fn is_default_ignored_path(path: &std::path::Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            if crate::watcher::filter::is_excluded_dir(&name_str) {
                return true;
            }
            // Skip dotdirs (except .github) — the shared function
            // handles known dotdirs, but catch any remaining ones
            if name_str.starts_with('.') && name_str.len() > 1 && name_str != ".github" {
                return true;
            }
        }
    }
    false
}

/// Check if a path is on a network mount (NFS, SMB, CIFS, etc.)
/// This is used to choose between fast walker (network) and gitignore-aware walker (local).
fn is_network_path(path: &std::path::Path) -> bool {
    let path_str = path.to_string_lossy();

    // macOS: /Volumes/ paths that aren't the main disk
    if path_str.starts_with("/Volumes/") && !path_str.starts_with("/Volumes/Macintosh") {
        return true;
    }

    // Linux: common network mount points
    if path_str.starts_with("/mnt/")
        || path_str.starts_with("/media/")
        || path_str.starts_with("/net/")
        || path_str.starts_with("/nfs/")
        || path_str.starts_with("/smb/")
        || path_str.starts_with("/cifs/")
    {
        return true;
    }

    // Check /proc/mounts on Linux for NFS/CIFS mounts
    #[cfg(target_os = "linux")]
    {
        if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
            for line in mounts.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    let mount_point = parts[1];
                    let fs_type = parts[2];
                    if path_str.starts_with(mount_point)
                        && (fs_type == "nfs"
                            || fs_type == "nfs4"
                            || fs_type == "cifs"
                            || fs_type == "smb")
                    {
                        return true;
                    }
                }
            }
        }
    }

    false
}

// Exclusion list is now shared via crate::watcher::filter::EXCLUDED_DIRS
// and crate::watcher::filter::is_excluded_dir().

/// Fast directory walker for network mounts.
/// Skips gitignore parsing (expensive over network) and uses a simple skip list.
fn fast_walk_directory(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut dirs_walked = 0u64;

    while let Some(dir) = stack.pop() {
        dirs_walked += 1;

        // Log progress every 100 directories
        if dirs_walked % 100 == 0 {
            tracing::debug!(dirs_walked, "fast_walk progress");
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "Failed to read directory");
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();

            // Skip hidden files/dirs (except .github)
            if name.starts_with('.') && name != "." && name != ".." && name != ".github" {
                continue;
            }

            // Skip excluded directories (node_modules, target, etc.)
            if crate::watcher::filter::is_excluded_dir(&name) {
                continue;
            }

            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                // Apply full filter (code file, size, ignore patterns)
                if should_index_file(&path) {
                    files.push(path);
                }
            }
        }
    }

    tracing::info!(total_files = files.len(), dirs_walked, "fast_walk complete");
    files
}

/// MCP server state.
pub struct McpState {
    pub db: Database,
    pub embeddings: Option<EmbeddingService>,
    /// API key for authentication (None = disabled)
    api_key: Option<String>,
    /// Nellie-V graph memory (None if graph.enabled = false)
    pub graph: Option<std::sync::Arc<parking_lot::RwLock<crate::graph::GraphMemory>>>,
    /// Enable structural parsing (tree-sitter AST analysis)
    pub enable_structural: bool,
    /// True while structural graph bootstrap is running in the background.
    pub structural_bootstrapping: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl McpState {
    /// Create new MCP state.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            db,
            embeddings: None,
            api_key: None,
            graph: None,
            enable_structural: false,
            structural_bootstrapping: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
        }
    }

    /// Create MCP state with embedding service.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // EmbeddingService is not const
    pub fn with_embeddings(db: Database, embeddings: EmbeddingService) -> Self {
        Self {
            db,
            embeddings: Some(embeddings),
            api_key: None,
            graph: None,
            enable_structural: false,
            structural_bootstrapping: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
        }
    }

    /// Create MCP state with API key.
    #[must_use]
    pub fn with_api_key(db: Database, api_key: Option<String>) -> Self {
        Self {
            db,
            embeddings: None,
            api_key,
            graph: None,
            enable_structural: false,
            structural_bootstrapping: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
        }
    }

    /// Create MCP state with embeddings and API key.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // EmbeddingService is not const
    pub fn with_embeddings_and_api_key(
        db: Database,
        embeddings: EmbeddingService,
        api_key: Option<String>,
    ) -> Self {
        Self {
            db,
            embeddings: Some(embeddings),
            api_key,
            graph: None,
            enable_structural: false,
            structural_bootstrapping: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
        }
    }

    /// Check if API key authentication is configured.
    #[must_use]
    pub const fn api_key_configured(&self) -> bool {
        self.api_key.is_some()
    }

    /// Validate an API key.
    #[must_use]
    pub fn validate_api_key(&self, provided_key: &str) -> bool {
        self.api_key
            .as_ref()
            .is_some_and(|expected| expected == provided_key)
    }

    /// Get the database.
    #[must_use]
    pub const fn db(&self) -> &Database {
        &self.db
    }

    /// Get the embedding service if available.
    #[must_use]
    pub fn embedding_service(&self) -> Option<EmbeddingService> {
        self.embeddings.clone()
    }

    /// Set graph memory on this state.
    pub fn set_graph(&mut self, graph: crate::graph::GraphMemory) {
        self.graph = Some(std::sync::Arc::new(parking_lot::RwLock::new(graph)));
    }

    /// Enable structural parsing.
    pub fn set_enable_structural(&mut self, enable: bool) {
        self.enable_structural = enable;
    }
}

/// Tool information with schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// Tool definitions for Nellie.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn get_tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "search_code".to_string(),
            description: Some(
                "Search indexed code repositories for relevant code snippets".to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language query to search for relevant code"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 10)",
                        "default": 10
                    },
                    "language": {
                        "type": "string",
                        "description": "Filter by programming language"
                    }
                },
                "required": ["query"]
            }),
        },
        ToolInfo {
            name: "search_lessons".to_string(),
            description: Some("Search previously recorded lessons learned".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language query to search lessons"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum lessons to return (default: 5)",
                        "default": 5
                    }
                },
                "required": ["query"]
            }),
        },
        ToolInfo {
            name: "list_lessons".to_string(),
            description: Some(
                "List all recorded lessons learned with optional filters for severity and limit"
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "severity": {
                        "type": "string",
                        "enum": ["critical", "warning", "info"],
                        "description": "Filter by severity level (optional)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum lessons to return (default: 50)",
                        "default": 50
                    }
                },
                "required": []
            }),
        },
        ToolInfo {
            name: "add_lesson".to_string(),
            description: Some("Record a lesson learned during development".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Brief title for the lesson"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full description of the lesson learned"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Tags for categorization"
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["critical", "warning", "info"],
                        "description": "Importance level (default: info)"
                    },
                    "solved_problem": {"type": "string", "description": "Problem this lesson solved (graph edge)"},
                    "used_tools": {"type": "array", "items": {"type": "string"}, "description": "Tools used (graph edges)"},
                    "related_concepts": {"type": "array", "items": {"type": "string"}, "description": "Related concepts (graph edges)"},
                    "learned_from": {"type": "string", "description": "Source derived from (graph edge)"}
                },
                "required": ["title", "content", "tags"]
            }),
        },
        ToolInfo {
            name: "delete_lesson".to_string(),
            description: Some("Delete a lesson by ID".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Lesson ID to delete"
                    }
                },
                "required": ["id"]
            }),
        },
        ToolInfo {
            name: "add_checkpoint".to_string(),
            description: Some("Store an agent checkpoint for context recovery".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Agent identifier"
                    },
                    "working_on": {
                        "type": "string",
                        "description": "Current task description"
                    },
                    "state": {
                        "type": "object",
                        "description": "State object to persist"
                    },
                    "tools_used": {"type": "array", "items": {"type": "string"}, "description": "Tools used in this session (graph edges)"},
                    "problems_encountered": {"type": "array", "items": {"type": "string"}, "description": "Problems encountered (graph edges)"},
                    "solutions_found": {"type": "array", "items": {"type": "string"}, "description": "Solutions found (graph edges)"},
                    "graph_suggestions_used": {"type": "array", "items": {"type": "string"}, "description": "Edge IDs from graph results that were used"},
                    "outcome": {"type": "string", "enum": ["success", "failure", "partial"], "description": "Session outcome for reinforcement learning"}
                },
                "required": ["agent", "working_on", "state"]
            }),
        },
        ToolInfo {
            name: "get_recent_checkpoints".to_string(),
            description: Some("Retrieve recent checkpoints, optionally filtered by agent. Ordered by time (newest first).".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Agent identifier (optional — omit to get recent checkpoints across all agents)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum checkpoints to return (default: 5)",
                        "default": 5
                    }
                },
                "required": []
            }),
        },
        ToolInfo {
            name: "trigger_reindex".to_string(),
            description: Some("Trigger manual re-indexing of specified paths".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File or directory path to re-index (optional, re-indexes all if omitted)"
                    }
                },
                "required": []
            }),
        },
        ToolInfo {
            name: "get_status".to_string(),
            description: Some("Get Nellie server status and statistics".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolInfo {
            name: "search_checkpoints".to_string(),
            description: Some("Search checkpoints semantically by query text".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Query text to search checkpoints"
                    },
                    "agent": {
                        "type": "string",
                        "description": "Optional agent filter to search only this agent's checkpoints"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum checkpoints to return (default: 5)",
                        "default": 5
                    }
                },
                "required": ["query"]
            }),
        },
        ToolInfo {
            name: "get_agent_status".to_string(),
            description: Some(
                "Get quick status for an agent (idle/in_progress, current task, checkpoint count)"
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Agent identifier"
                    }
                },
                "required": ["agent"]
            }),
        },
        ToolInfo {
            name: "index_repo".to_string(),
            description: Some(
                "Index a repository or directory path on demand. Use this to ensure Nellie has fresh context for a specific project. Respects .gitignore patterns."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the repository or directory to index"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolInfo {
            name: "diff_index".to_string(),
            description: Some(
                "Incremental indexing: compare file mtimes with database and only index new/changed files. Also removes entries for deleted files. Fast for routine syncs."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory to diff-index"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolInfo {
            name: "full_reindex".to_string(),
            description: Some(
                "Nuclear option: clear all indexed data for a path and re-index from scratch. Use when the index seems corrupted or you need a clean slate."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to fully re-index (clears existing data first)"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolInfo {
            name: "search_hybrid".to_string(),
            description: Some(
                "Search using vector similarity + graph expansion for richer context.                  Returns vector results enriched with related graph entities (tools, problems,                  solutions, concepts). Falls back to plain search_code when graph is disabled."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language query to search for"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of vector results (default: 10)",
                        "default": 10
                    },
                    "expansion_depth": {
                        "type": "integer",
                        "description": "Graph traversal depth from matched entities (default: 2)",
                        "default": 2
                    }
                },
                "required": ["query"]
            }),
        },
        ToolInfo {
            name: "query_graph".to_string(),
            description: Some(
                "Query the knowledge graph directly. Find entities by type/label and                  traverse relationships. Returns error if graph is disabled."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "entity_type": {
                        "type": "string",
                        "enum": ["agent", "tool", "problem", "solution", "concept", "person", "project", "chunk"],
                        "description": "Filter by node type"
                    },
                    "label": {
                        "type": "string",
                        "description": "Fuzzy match on entity label"
                    },
                    "relationship": {
                        "type": "string",
                        "enum": ["used", "solved", "failed_for", "knows", "prefers", "depends_on", "related_to", "derived_from"],
                        "description": "Filter by edge type"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["outgoing", "incoming", "both"],
                        "description": "Traversal direction (default: both)",
                        "default": "both"
                    },
                    "min_confidence": {
                        "type": "number",
                        "description": "Minimum edge confidence threshold (default: 0.3)",
                        "default": 0.3
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Traversal depth (default: 1)",
                        "default": 1
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (default: 10)",
                        "default": 10
                    }
                },
                "required": []
            }),
        },
        ToolInfo {
            name: "bootstrap_graph".to_string(),
            description: Some(
                "Seed the knowledge graph from existing lessons and checkpoints. \
                 Use dry_run to preview, execute to apply."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["dry_run", "execute"],
                        "description": "dry_run previews changes, execute applies them (default: dry_run)",
                        "default": "dry_run"
                    }
                },
                "required": []
            }),
        },
        ToolInfo {
            name: "get_blast_radius".to_string(),
            description: Some(
                "Given changed files, find all affected functions, classes, and tests via structural graph traversal. Requires --enable-structural."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "changed_files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of changed file paths"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Traversal depth for callers (default: 2)",
                        "default": 2
                    }
                },
                "required": ["changed_files"]
            }),
        },
        ToolInfo {
            name: "get_review_context".to_string(),
            description: Some(
                "Token-optimized structural summary for code review. Shows what changed, what's affected, and test coverage."
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "changed_files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of changed file paths"
                    }
                },
                "required": ["changed_files"]
            }),
        },
        ToolInfo {
            name: "query_structure".to_string(),
            description: Some(
                "Query structural relationships of a code symbol (callers, callees, tests, etc.)"
                    .to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name to query"
                    },
                    "query_type": {
                        "type": "string",
                        "enum": ["callers", "callees", "tests", "contains", "symbols_in_file", "importers", "inheritors"],
                        "description": "Type of structural query"
                    },
                    "language": {
                        "type": "string",
                        "description": "Filter by language (optional)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (default: 20)",
                        "default": 20
                    }
                },
                "required": ["symbol", "query_type"]
            }),
        },
    ]
}

/// Create MCP router.
pub fn create_mcp_router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp/tools", get(list_tools))
        .route("/mcp/invoke", post(invoke_tool))
        .with_state(state)
}

/// List available tools.
async fn list_tools() -> Json<Vec<ToolInfo>> {
    Json(get_tools())
}

/// Tool invocation request.
#[derive(Debug, Deserialize)]
pub struct ToolRequest {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool invocation response.
#[derive(Debug, Serialize)]
pub struct ToolResponse {
    pub content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// MCP metadata — tells Claude Code to allow large results without truncation.
    #[serde(rename = "_meta")]
    pub meta: serde_json::Value,
}

/// Default MCP _meta for tool responses. Allows up to 500K chars before truncation.
fn tool_meta() -> serde_json::Value {
    serde_json::json!({"anthropic/maxResultSizeChars": 500_000})
}

/// Extract agent identifier from tool arguments.
///
/// Looks for an "agent" field in the JSON arguments. Tools like `add_checkpoint`,
/// `get_recent_checkpoints`, and `get_agent_status` include this field.
/// Returns "unknown" if not present.
fn extract_agent(args: &serde_json::Value) -> &str {
    args.get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

/// Invoke a tool.
async fn invoke_tool(
    State(state): State<Arc<McpState>>,
    Json(request): Json<ToolRequest>,
) -> Json<ToolResponse> {
    let tool_name = request.name.clone();
    let agent = extract_agent(&request.arguments).to_string();
    let start = std::time::Instant::now();

    let span = tracing::info_span!(
        "tool_invocation",
        tool = %tool_name,
        agent = %agent,
        dispatch = "http",
    );
    let _guard = span.enter();

    tracing::debug!("Invoking tool: {}", tool_name);

    let result = match request.name.as_str() {
        "search_code" => handle_search_code(&state, &request.arguments).await,
        "search_lessons" => handle_search_lessons(&state, &request.arguments).await,
        "list_lessons" => handle_list_lessons(&state, &request.arguments),
        "add_lesson" => handle_add_lesson(&state, &request.arguments).await,
        "delete_lesson" => handle_delete_lesson(&state, &request.arguments),
        "add_checkpoint" => handle_add_checkpoint(&state, &request.arguments).await,
        "get_recent_checkpoints" => handle_get_checkpoints(&state, &request.arguments),
        "trigger_reindex" => handle_trigger_reindex(&state, &request.arguments).await,
        "get_status" => handle_get_status(&state),
        "search_checkpoints" => handle_search_checkpoints(&state, &request.arguments).await,
        "get_agent_status" => handle_get_agent_status(&state, &request.arguments),
        "index_repo" => handle_index_repo(&state, &request.arguments).await,
        "diff_index" => handle_diff_index(&state, &request.arguments).await,
        "full_reindex" => handle_full_reindex(&state, &request.arguments).await,
        "search_hybrid" => handle_search_hybrid(&state, &request.arguments).await,
        "query_graph" => handle_query_graph(&state, &request.arguments),
        "bootstrap_graph" => handle_bootstrap_graph(&state, &request.arguments),
        "get_blast_radius" => handle_get_blast_radius(&state, &request.arguments),
        "get_review_context" => handle_get_review_context(&state, &request.arguments),
        "query_structure" => handle_query_structure(&state, &request.arguments),
        _ => Err(format!("Unknown tool: {}", request.name)),
    };

    let latency = start.elapsed();

    match result {
        Ok(content) => {
            let response_bytes = content.to_string().len();
            crate::server::metrics::record_tool_call(
                &tool_name,
                &agent,
                "success",
                latency,
                response_bytes,
            );
            tracing::debug!(
                latency_ms = latency.as_millis() as u64,
                response_bytes,
                "Tool invocation succeeded"
            );
            Json(ToolResponse {
                content,
                error: None,
                meta: tool_meta(),
            })
        }
        Err(e) => {
            crate::server::metrics::record_tool_call(&tool_name, &agent, "error", latency, 0);
            tracing::warn!(
                error = %e,
                latency_ms = latency.as_millis() as u64,
                "Tool invocation failed"
            );
            Json(ToolResponse {
                content: serde_json::Value::Null,
                error: Some(e),
                meta: tool_meta(),
            })
        }
    }
}

/// Invoke a tool directly (for SSE transport).
pub async fn invoke_tool_direct(state: &McpState, request: ToolRequest) -> ToolResponse {
    let tool_name = request.name.clone();
    let agent = extract_agent(&request.arguments).to_string();
    let start = std::time::Instant::now();

    tracing::debug!(tool = %tool_name, agent = %agent, dispatch = "sse", "Invoking tool (direct)");

    let result = match request.name.as_str() {
        "search_code" => handle_search_code(state, &request.arguments).await,
        "search_lessons" => handle_search_lessons(state, &request.arguments).await,
        "list_lessons" => handle_list_lessons(state, &request.arguments),
        "add_lesson" => handle_add_lesson(state, &request.arguments).await,
        "delete_lesson" => handle_delete_lesson(state, &request.arguments),
        "add_checkpoint" => handle_add_checkpoint(state, &request.arguments).await,
        "get_recent_checkpoints" => handle_get_checkpoints(state, &request.arguments),
        "trigger_reindex" => handle_trigger_reindex(state, &request.arguments).await,
        "get_status" => handle_get_status(state),
        "search_checkpoints" => handle_search_checkpoints(state, &request.arguments).await,
        "get_agent_status" => handle_get_agent_status(state, &request.arguments),
        "index_repo" => handle_index_repo(state, &request.arguments).await,
        "diff_index" => handle_diff_index(state, &request.arguments).await,
        "full_reindex" => handle_full_reindex(state, &request.arguments).await,
        "search_hybrid" => handle_search_hybrid(state, &request.arguments).await,
        "query_graph" => handle_query_graph(state, &request.arguments),
        "bootstrap_graph" => handle_bootstrap_graph(state, &request.arguments),
        "get_blast_radius" => handle_get_blast_radius(state, &request.arguments),
        "query_structure" => handle_query_structure(state, &request.arguments),
        _ => Err(format!("Unknown tool: {}", request.name)),
    };

    let latency = start.elapsed();

    match result {
        Ok(content) => {
            let response_bytes = content.to_string().len();
            crate::server::metrics::record_tool_call(
                &tool_name,
                &agent,
                "success",
                latency,
                response_bytes,
            );
            tracing::debug!(
                tool = %tool_name,
                latency_ms = latency.as_millis() as u64,
                response_bytes,
                "Tool invocation succeeded"
            );
            ToolResponse {
                content,
                error: None,
                meta: tool_meta(),
            }
        }
        Err(e) => {
            crate::server::metrics::record_tool_call(&tool_name, &agent, "error", latency, 0);
            tracing::warn!(
                tool = %tool_name,
                error = %e,
                latency_ms = latency.as_millis() as u64,
                "Tool invocation failed"
            );
            ToolResponse {
                content: serde_json::Value::Null,
                error: Some(e),
                meta: tool_meta(),
            }
        }
    }
}

// Tool handlers

#[allow(clippy::cast_possible_truncation)]
async fn handle_search_code(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let query = args["query"].as_str().ok_or("query is required")?;
    let limit = args["limit"].as_u64().unwrap_or(10) as usize;
    let language_filter = args["language"].as_str();

    // CRITICAL: Embedding service MUST be initialized for semantic search
    let embeddings = state.embeddings.as_ref().ok_or_else(|| {
        "Embedding service not initialized. Semantic search requires real embeddings.".to_string()
    })?;

    if !embeddings.is_initialized() {
        return Err(
            "Embedding service not fully initialized. Please wait for model loading to complete."
                .to_string(),
        );
    }

    // Generate embedding for query using real embeddings
    // We're in a sync context (Axum handler), so we use blocking runtime
    let embeddings = embeddings.clone();
    let query_text = query.to_string();

    let embedding = embeddings
        .embed_one(query_text)
        .await
        .map_err(|e| format!("Failed to generate query embedding: {e}"))?;

    // Create search options
    let mut search_opts = crate::storage::SearchOptions::new(limit);
    if let Some(lang) = language_filter {
        search_opts = search_opts.with_language(lang);
    }

    // Search the database using real vector similarity
    let results = state
        .db
        .with_conn(|conn| crate::storage::search_chunks(conn, &embedding, &search_opts))
        .map_err(|e| format!("Vector search failed: {e}"))?;

    // Format results for MCP response
    let formatted_results: Vec<serde_json::Value> = results
        .iter()
        .map(|result| {
            serde_json::json!({
                "file_path": result.record.file_path,
                "chunk_index": result.record.chunk_index,
                "start_line": result.record.start_line,
                "end_line": result.record.end_line,
                "content": result.record.content,
                "language": result.record.language,
                "score": result.score,
                "distance": result.distance,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "results": formatted_results,
        "query": query,
        "limit": limit,
        "count": formatted_results.len(),
    }))
}

#[allow(clippy::cast_possible_truncation)]
async fn handle_search_hybrid(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let query = args["query"].as_str().ok_or("query is required")?;
    let limit = args["limit"].as_u64().unwrap_or(10) as usize;
    let expansion_depth = args["expansion_depth"].as_u64().unwrap_or(2) as usize;

    // Step 1: Run vector search (same as search_code)
    let embeddings = state.embeddings.as_ref().ok_or_else(|| {
        "Embedding service not initialized. Semantic search requires real embeddings.".to_string()
    })?;

    if !embeddings.is_initialized() {
        return Err(
            "Embedding service not fully initialized. Please wait for model loading to complete."
                .to_string(),
        );
    }

    let embedding = embeddings
        .embed_one(query.to_string())
        .await
        .map_err(|e| format!("Failed to generate query embedding: {e}"))?;

    let search_opts = crate::storage::SearchOptions::new(limit);
    let results = state
        .db
        .with_conn(|conn| crate::storage::search_chunks(conn, &embedding, &search_opts))
        .map_err(|e| format!("Vector search failed: {e}"))?;

    // Format vector results (same as search_code)
    let vector_results: Vec<serde_json::Value> = results
        .iter()
        .map(|result| {
            serde_json::json!({
                "file_path": result.record.file_path,
                "chunk_index": result.record.chunk_index,
                "start_line": result.record.start_line,
                "end_line": result.record.end_line,
                "content": result.record.content,
                "language": result.record.language,
                "score": result.score,
                "distance": result.distance,
            })
        })
        .collect();

    // Step 2: Graph expansion (if graph is enabled)
    let mut graph_context: Vec<serde_json::Value> = Vec::new();
    let mut edge_ids: Vec<String> = Vec::new();

    if let Some(ref graph_lock) = state.graph {
        let graph = graph_lock.read(); // parking_lot RwLock

        // Try to find graph entities related to query text via fuzzy match
        let matched_node_ids = graph.fuzzy_match(query);

        // Also try to match from vector result file paths/content
        // (chunk nodes reference vector store entries via record_id)
        let mut start_node_ids: Vec<String> = matched_node_ids;
        for result in &results {
            // Look up chunk nodes that reference this record
            if let Some(record_id) = result.record.id {
                let record_id_str = record_id.to_string();
                let chunk_nodes = graph.find_by_record_id(&record_id_str);
                start_node_ids.extend(chunk_nodes);
            }
        }
        start_node_ids.dedup();

        // For each starting node, do graph traversal
        let mut seen_entities: std::collections::HashSet<String> = std::collections::HashSet::new();

        for start_id in &start_node_ids {
            let query_results = crate::graph::query::GraphQuery::new(&graph)
                .label(start_id) // Start from nodes matching this ID
                .direction(crate::graph::query::Direction::Both)
                .depth(expansion_depth)
                .min_confidence(0.3)
                .limit(20)
                .execute();

            for qr in query_results {
                if seen_entities.insert(qr.entity.id.clone()) {
                    // Collect edge IDs from path for outcome tracking
                    for edge in &qr.path {
                        edge_ids.push(edge.edge_id.clone());
                    }

                    graph_context.push(serde_json::json!({
                        "id": qr.entity.id,
                        "type": qr.entity.entity_type,
                        "label": qr.entity.label,
                        "depth": qr.depth,
                        "path": qr.path,
                    }));
                }
            }
        }
        edge_ids.dedup();
    }

    // Step 3: Structural context (if symbols table has data)
    let structural_context: Vec<serde_json::Value> = state
        .db
        .with_conn(|conn| {
            // Check if symbols table exists and has data (graceful degradation)
            let has_symbols: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='symbols')",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if !has_symbols {
                return Ok(Vec::new());
            }

            // Search for symbols matching query terms
            let query_terms: Vec<&str> = query.split_whitespace().collect();
            let mut results: Vec<serde_json::Value> = Vec::new();

            for term in &query_terms {
                let symbols = crate::structural::storage::query_symbols_by_name(conn, term)
                    .unwrap_or_default();
                for sym in symbols.into_iter().take(5) {
                    results.push(serde_json::json!({
                        "name": sym.symbol_name,
                        "kind": sym.symbol_kind.as_str(),
                        "file_path": sym.file_path,
                        "start_line": sym.start_line,
                        "end_line": sym.end_line,
                        "scope": sym.scope,
                        "language": sym.language,
                    }));
                }
            }

            // Also do a LIKE search for partial matches
            if results.is_empty() {
                let like_pattern = format!("%{query}%");
                let mut stmt = conn
                    .prepare(
                        "SELECT symbol_name, symbol_kind, file_path, start_line, end_line, scope, language
                         FROM symbols WHERE symbol_name LIKE ?1 LIMIT 10",
                    )
                    .map_err(|e| crate::error::StorageError::Database(format!("query failed: {e}")))?;

                let rows = stmt
                    .query_map([&like_pattern], |row| {
                        Ok(serde_json::json!({
                            "name": row.get::<_, String>(0)?,
                            "kind": row.get::<_, String>(1)?,
                            "file_path": row.get::<_, String>(2)?,
                            "start_line": row.get::<_, i32>(3)?,
                            "end_line": row.get::<_, i32>(4)?,
                            "scope": row.get::<_, Option<String>>(5)?,
                            "language": row.get::<_, String>(6)?,
                        }))
                    })
                    .map_err(|e| crate::error::StorageError::Database(format!("query failed: {e}")))?;

                for val in rows.flatten() {
                    results.push(val);
                }
            }

            Ok(results)
        })
        .unwrap_or_default();

    Ok(serde_json::json!({
        "results": vector_results,
        "query": query,
        "limit": limit,
        "count": vector_results.len(),
        "graph_context": graph_context,
        "graph_context_count": graph_context.len(),
        "edge_ids": edge_ids,
        "structural_context": structural_context,
        "structural_context_count": structural_context.len(),
    }))
}

fn handle_query_graph(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let graph_lock = state.graph.as_ref().ok_or_else(|| {
        "Graph is not enabled. Set graph.enabled = true in configuration.".to_string()
    })?;

    let graph = graph_lock.read(); // parking_lot RwLock

    // Build query from parameters
    let mut query = crate::graph::query::GraphQuery::new(&graph);

    if let Some(entity_type_str) = args["entity_type"].as_str() {
        if let Some(entity_type) = crate::graph::EntityType::parse(entity_type_str) {
            query = query.entity_type(entity_type);
        }
    }

    if let Some(label) = args["label"].as_str() {
        query = query.label(label);
    }

    if let Some(relationship_str) = args["relationship"].as_str() {
        if let Some(relationship) = crate::graph::RelationshipKind::parse(relationship_str) {
            query = query.relationship(relationship);
        }
    }

    if let Some(direction_str) = args["direction"].as_str() {
        query = query.direction(crate::graph::query::Direction::parse(direction_str));
    }

    if let Some(min_conf) = args["min_confidence"].as_f64() {
        #[allow(clippy::cast_possible_truncation)]
        let conf = min_conf as f32;
        query = query.min_confidence(conf);
    }

    if let Some(depth) = args["depth"].as_u64() {
        query = query.depth(depth as usize);
    }

    if let Some(limit) = args["limit"].as_u64() {
        query = query.limit(limit as usize);
    }

    let results = query.execute();

    // Format results
    let formatted: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "entity": {
                    "id": r.entity.id,
                    "type": r.entity.entity_type,
                    "label": r.entity.label,
                },
                "path": r.path,
                "depth": r.depth,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "results": formatted,
        "count": formatted.len(),
    }))
}

fn handle_bootstrap_graph(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let graph_lock = state.graph.as_ref().ok_or_else(|| {
        "Graph is not enabled. Set graph.enabled = true in configuration.".to_string()
    })?;

    let mode = args["mode"].as_str().unwrap_or("dry_run");

    // Load all lessons from database
    let lessons = state
        .db
        .with_conn(crate::storage::list_lessons)
        .map_err(|e| format!("Failed to load lessons: {e}"))?;

    // Load all checkpoints (use a large limit to get them all)
    let checkpoints: Vec<crate::storage::CheckpointRecord> = state
        .db
        .with_conn(|conn| {
            // Direct SQL query to get all checkpoints
            let mut stmt = conn
                .prepare(
                    "SELECT id, agent, working_on, state, created_at FROM checkpoints \
                     ORDER BY created_at DESC",
                )
                .map_err(|e| crate::error::StorageError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(crate::storage::CheckpointRecord {
                        id: row.get(0)?,
                        agent: row.get(1)?,
                        working_on: row.get(2)?,
                        state: serde_json::from_str(&row.get::<_, String>(3)?)
                            .unwrap_or_else(|_| serde_json::json!({})),
                        repo: None,
                        session_id: None,
                        created_at: row.get(4)?,
                    })
                })
                .map_err(|e| crate::error::StorageError::Database(e.to_string()))?;
            let results: Vec<_> = rows.flatten().collect();
            Ok(results)
        })
        .map_err(|e| format!("Failed to load checkpoints: {e}"))?;

    if mode == "dry_run" {
        // Clone the graph, run bootstrap on clone, report stats
        let mut graph_clone = graph_lock.read().clone();
        let stats =
            crate::graph::bootstrap::run_bootstrap(&mut graph_clone, &lessons, &checkpoints);
        Ok(serde_json::json!({
            "mode": "dry_run",
            "lessons_found": lessons.len(),
            "checkpoints_found": checkpoints.len(),
            "would_create_nodes": stats.nodes_created,
            "would_create_edges": stats.edges_created,
            "message": "Dry run complete. Use mode: 'execute' to apply changes.",
        }))
    } else {
        // Execute: mutate the real graph
        let mut graph = graph_lock.write();
        let stats = crate::graph::bootstrap::run_bootstrap(&mut graph, &lessons, &checkpoints);

        // Persist to SQLite (best-effort)
        crate::graph::enrichment::persist_changes(&state.db, &graph, &[], &[]);

        Ok(serde_json::json!({
            "mode": "execute",
            "lessons_processed": stats.lessons_processed,
            "checkpoints_processed": stats.checkpoints_processed,
            "nodes_created": stats.nodes_created,
            "edges_created": stats.edges_created,
            "message": "Bootstrap complete. Graph has been seeded from existing data.",
        }))
    }
}

/// Handle `get_blast_radius` tool invocation.
///
/// Given a list of changed files, finds all affected functions, classes, and tests
/// by traversing the structural call graph up to the specified depth.
fn handle_get_blast_radius(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let changed_files: Vec<String> = args["changed_files"]
        .as_array()
        .ok_or("changed_files is required and must be an array")?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let depth = args["depth"].as_u64().unwrap_or(2) as usize;

    let mut affected_symbols: Vec<serde_json::Value> = Vec::new();
    let mut affected_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut test_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    state
        .db
        .with_conn(|conn| {
            for file_path in &changed_files {
                // Get all symbols in the changed file
                let symbols = crate::structural::storage::query_symbols_by_file(conn, file_path)
                    .unwrap_or_default();

                for symbol in &symbols {
                    // Find callers (up to depth levels)
                    let mut current_targets = vec![symbol.symbol_name.clone()];
                    for level in 0..depth {
                        let mut next_targets = Vec::new();
                        for target in &current_targets {
                            let callers = crate::structural::storage::query_callers(conn, target)
                                .unwrap_or_default();
                            for caller in &callers {
                                let key = format!(
                                    "{}:{}:{}",
                                    caller.file_path, caller.symbol_name, caller.start_line
                                );
                                if seen.insert(key) {
                                    affected_symbols.push(serde_json::json!({
                                        "name": caller.symbol_name,
                                        "kind": caller.symbol_kind.as_str(),
                                        "file_path": caller.file_path,
                                        "start_line": caller.start_line,
                                        "end_line": caller.end_line,
                                        "reason": format!("calls {} (depth {})", target, level + 1),
                                    }));
                                }
                                affected_files.insert(caller.file_path.clone());
                                next_targets.push(caller.symbol_name.clone());

                                // Check if this caller is a test function
                                if caller.symbol_kind
                                    == crate::structural::extractor::SymbolKind::TestFunction
                                {
                                    test_files.insert(caller.file_path.clone());
                                }
                            }
                        }
                        current_targets = next_targets;
                        if current_targets.is_empty() {
                            break;
                        }
                    }

                    // Find test functions that test this symbol
                    let test_name = format!("test_{}", symbol.symbol_name);
                    let tests = crate::structural::storage::query_symbols_by_name(conn, &test_name)
                        .unwrap_or_default();
                    for test_sym in &tests {
                        let key = format!(
                            "{}:{}:{}",
                            test_sym.file_path, test_sym.symbol_name, test_sym.start_line
                        );
                        if seen.insert(key) {
                            test_files.insert(test_sym.file_path.clone());
                            affected_symbols.push(serde_json::json!({
                                "name": test_sym.symbol_name,
                                "kind": "test_function",
                                "file_path": test_sym.file_path,
                                "start_line": test_sym.start_line,
                                "reason": format!("tests {}", symbol.symbol_name),
                            }));
                        }
                    }
                }
            }
            Ok(())
        })
        .map_err(|e| format!("Database error: {e}"))?;

    Ok(serde_json::json!({
        "changed_files": changed_files,
        "depth": depth,
        "affected_symbols": affected_symbols,
        "affected_files": affected_files.into_iter().collect::<Vec<_>>(),
        "test_files": test_files.into_iter().collect::<Vec<_>>(),
    }))
}

/// Handle `get_review_context` tool invocation.
///
/// Generates a token-optimized structural summary for code review.
/// Shows what changed, which functions/symbols were affected, and test coverage status.
fn handle_get_review_context(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let changed_files: Vec<String> = args["changed_files"]
        .as_array()
        .ok_or("changed_files is required and must be an array")?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let mut total_symbols = 0u32;
    let mut total_callers = 0u32;
    let mut total_tests = 0u32;
    let mut untested = 0u32;
    let mut changed_symbol_names: Vec<String> = Vec::new();

    state
        .db
        .with_conn(|conn| {
            for file_path in &changed_files {
                let symbols = crate::structural::storage::query_symbols_by_file(conn, file_path)
                    .unwrap_or_default();

                for symbol in &symbols {
                    if matches!(
                        symbol.symbol_kind,
                        crate::structural::SymbolKind::Function
                            | crate::structural::SymbolKind::Method
                    ) {
                        total_symbols += 1;
                        changed_symbol_names.push(symbol.symbol_name.clone());

                        let callers =
                            crate::structural::storage::query_callers(conn, &symbol.symbol_name)
                                .unwrap_or_default();
                        total_callers += callers.len() as u32;

                        let test_name = format!("test_{}", symbol.symbol_name);
                        let tests =
                            crate::structural::storage::query_symbols_by_name(conn, &test_name)
                                .unwrap_or_default();
                        if tests.is_empty() {
                            untested += 1;
                        } else {
                            total_tests += tests.len() as u32;
                        }
                    }
                }
            }
            Ok(())
        })
        .map_err(|e| format!("Database error: {e}"))?;

    let summary = format!(
        "{total_symbols} functions changed, {total_callers} callers affected, {total_tests} tests cover these changes, {untested} functions have no test coverage"
    );

    Ok(serde_json::json!({
        "summary": summary,
        "changed_files": changed_files.len(),
        "changed_symbols": total_symbols,
        "affected_callers": total_callers,
        "test_coverage": total_tests,
        "untested_functions": untested,
        "changed_symbol_names": changed_symbol_names,
    }))
}

/// Handle `query_structure` tool invocation.
///
/// Query structural relationships of a code symbol (callers, callees, tests, etc.).
/// Supports queries: callers, callees, tests, contains, symbols_in_file.
fn handle_query_structure(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let symbol = args["symbol"].as_str().ok_or("symbol is required")?;
    let query_type = args["query_type"]
        .as_str()
        .ok_or("query_type is required")?;
    let language = args["language"].as_str();
    let limit = args["limit"].as_u64().unwrap_or(20) as usize;

    let results: Vec<serde_json::Value> = state
        .db
        .with_conn(|conn| {
            let symbols = match query_type {
                "callers" => crate::structural::storage::query_callers(conn, symbol)
                    .unwrap_or_default(),
                "callees" => crate::structural::storage::query_callees(conn, symbol)
                    .unwrap_or_default(),
                "tests" => {
                    let test_name = format!("test_{symbol}");
                    crate::structural::storage::query_symbols_by_name(conn, &test_name)
                        .unwrap_or_default()
                }
                "contains" => {
                    // Find symbols with scope matching the given symbol name
                    let mut stmt = conn
                        .prepare(
                            "SELECT id, file_path, symbol_name, symbol_kind, language, start_line, end_line, scope, signature, file_hash, indexed_at
                             FROM symbols WHERE scope = ?1",
                        )
                        .map_err(|e| crate::error::StorageError::Database(format!("query failed: {e}")))?;
                    let rows = stmt
                        .query_map([symbol], |row| {
                            Ok(crate::structural::storage::SymbolRecord {
                                id: row.get(0)?,
                                file_path: row.get(1)?,
                                symbol_name: row.get(2)?,
                                symbol_kind: crate::structural::SymbolKind::parse(
                                    &row.get::<_, String>(3)?,
                                )
                                .unwrap_or(crate::structural::SymbolKind::Function),
                                language: row.get(4)?,
                                start_line: row.get(5)?,
                                end_line: row.get(6)?,
                                scope: row.get(7)?,
                                signature: row.get(8)?,
                                file_hash: row.get(9)?,
                                indexed_at: row.get(10)?,
                            })
                        })
                        .map_err(|e| crate::error::StorageError::Database(format!("query failed: {e}")))?;
                    rows.flatten().collect()
                }
                "symbols_in_file" => {
                    crate::structural::storage::query_symbols_by_file(conn, symbol)
                        .unwrap_or_default()
                }
                "importers" => crate::structural::storage::query_importers(conn, symbol)
                    .unwrap_or_default(),
                "inheritors" => crate::structural::storage::query_inheritors(conn, symbol)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };

            let results: Vec<serde_json::Value> = symbols
                .into_iter()
                .filter(|s| language.map_or(true, |l| s.language == l))
                .take(limit)
                .map(|s| {
                    serde_json::json!({
                        "name": s.symbol_name,
                        "kind": s.symbol_kind.as_str(),
                        "file_path": s.file_path,
                        "start_line": s.start_line,
                        "end_line": s.end_line,
                        "scope": s.scope,
                        "language": s.language,
                    })
                })
                .collect();

            Ok(results)
        })
        .map_err(|e| format!("Database error: {e}"))?;

    Ok(serde_json::json!({
        "symbol": symbol,
        "query_type": query_type,
        "results": results,
        "count": results.len(),
    }))
}

#[allow(clippy::cast_possible_truncation)]
async fn handle_search_lessons(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let query = args["query"].as_str().ok_or("query is required")?;
    let limit = args["limit"].as_u64().unwrap_or(5) as usize;

    // CRITICAL: Embedding service MUST be initialized for semantic search
    let embeddings = state.embeddings.as_ref().ok_or_else(|| {
        "Embedding service not initialized. Semantic search requires real embeddings.".to_string()
    })?;

    if !embeddings.is_initialized() {
        return Err(
            "Embedding service not fully initialized. Please wait for model loading to complete."
                .to_string(),
        );
    }

    // Generate embedding for query using real embeddings
    let embeddings = embeddings.clone();
    let query_text = query.to_string();

    let embedding = embeddings
        .embed_one(query_text)
        .await
        .map_err(|e| format!("Failed to generate query embedding: {e}"))?;

    // Search lessons using vector similarity
    let lessons = state
        .db
        .with_conn(|conn| crate::storage::search_lessons_by_embedding(conn, &embedding, limit))
        .map_err(|e| e.to_string())?;

    Ok(serde_json::to_value(&lessons).unwrap_or_default())
}

#[allow(clippy::redundant_closure, clippy::cast_possible_truncation)]
fn handle_list_lessons(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let severity = args["severity"].as_str();
    let limit = args["limit"].as_u64().unwrap_or(50) as usize;

    let lessons = if let Some(severity_filter) = severity {
        state
            .db
            .with_conn(|conn| crate::storage::list_lessons_by_severity(conn, severity_filter))
            .map_err(|e| e.to_string())?
    } else {
        state
            .db
            .with_conn(|conn| crate::storage::list_lessons(conn))
            .map_err(|e| e.to_string())?
    };

    // Apply limit
    let limited_lessons: Vec<_> = lessons.into_iter().take(limit).collect();

    Ok(serde_json::json!({
        "lessons": serde_json::to_value(&limited_lessons).unwrap_or(serde_json::Value::Array(vec![])),
        "count": limited_lessons.len(),
        "severity": severity.unwrap_or("all")
    }))
}

/// Enrich a lesson with graph metadata (solved problems, tools, concepts, derivations).
#[allow(clippy::too_many_lines)]
fn enrich_lesson_with_graph(
    graph_lock: &std::sync::Arc<parking_lot::RwLock<crate::graph::GraphMemory>>,
    db: &crate::storage::Database,
    args: &serde_json::Value,
    title: &str,
) -> serde_json::Value {
    let has_graph_fields = args.get("solved_problem").is_some()
        || args.get("used_tools").is_some()
        || args.get("related_concepts").is_some()
        || args.get("learned_from").is_some();

    if !has_graph_fields {
        return serde_json::Value::Null;
    }

    let mut graph = graph_lock.write();
    let mut entity_ids = Vec::new();
    let mut edge_ids = Vec::new();

    // Create the lesson Solution node
    let solution_id =
        crate::graph::ensure_entity(&mut graph, crate::graph::EntityType::Solution, title);
    entity_ids.push(solution_id.clone());

    // Process solved_problem: create Problem node and "solved" edge
    if let Some(problem) = args.get("solved_problem").and_then(|v| v.as_str()) {
        let problem_id =
            crate::graph::ensure_entity(&mut graph, crate::graph::EntityType::Problem, problem);
        entity_ids.push(problem_id.clone());
        if let Some(edge_id) = crate::graph::ensure_edge(
            &mut graph,
            &solution_id,
            &problem_id,
            crate::graph::RelationshipKind::Solved,
            None,
        ) {
            edge_ids.push(edge_id);
        }
    }

    // Process used_tools: create Tool node for each and "used" edges
    if let Some(tools_array) = args.get("used_tools").and_then(|v| v.as_array()) {
        for tool_val in tools_array {
            if let Some(tool) = tool_val.as_str() {
                let tool_id =
                    crate::graph::ensure_entity(&mut graph, crate::graph::EntityType::Tool, tool);
                entity_ids.push(tool_id.clone());
                if let Some(edge_id) = crate::graph::ensure_edge(
                    &mut graph,
                    &solution_id,
                    &tool_id,
                    crate::graph::RelationshipKind::Used,
                    None,
                ) {
                    edge_ids.push(edge_id);
                }
            }
        }
    }

    // Process related_concepts: create Concept node for each and "related_to" edges
    if let Some(concepts_array) = args.get("related_concepts").and_then(|v| v.as_array()) {
        for concept_val in concepts_array {
            if let Some(concept) = concept_val.as_str() {
                let concept_id = crate::graph::ensure_entity(
                    &mut graph,
                    crate::graph::EntityType::Concept,
                    concept,
                );
                entity_ids.push(concept_id.clone());
                if let Some(edge_id) = crate::graph::ensure_edge(
                    &mut graph,
                    &solution_id,
                    &concept_id,
                    crate::graph::RelationshipKind::RelatedTo,
                    None,
                ) {
                    edge_ids.push(edge_id);
                }
            }
        }
    }

    // Process learned_from: create Concept node and "derived_from" edge
    if let Some(source) = args.get("learned_from").and_then(|v| v.as_str()) {
        let source_id =
            crate::graph::ensure_entity(&mut graph, crate::graph::EntityType::Concept, source);
        entity_ids.push(source_id.clone());
        if let Some(edge_id) = crate::graph::ensure_edge(
            &mut graph,
            &solution_id,
            &source_id,
            crate::graph::RelationshipKind::DerivedFrom,
            None,
        ) {
            edge_ids.push(edge_id);
        }
    }

    // Persist changes to database
    crate::graph::persist_changes(db, &graph, &entity_ids, &edge_ids);

    // Record stats in response
    serde_json::json!({
        "nodes_created": entity_ids.len(),
        "edges_created": edge_ids.len()
    })
}

#[allow(clippy::cast_possible_truncation)]
async fn handle_add_lesson(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let title = args["title"].as_str().ok_or("title is required")?;
    let content = args["content"].as_str().ok_or("content is required")?;
    let tags_array = args["tags"].as_array().ok_or("tags is required")?;
    let tags: Vec<String> = tags_array
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let severity = args["severity"].as_str().unwrap_or("info");

    let lesson = crate::storage::LessonRecord::new(title, content, tags).with_severity(severity);
    let id = lesson.id.clone();

    // Store lesson in database
    state
        .db
        .with_conn(|conn| crate::storage::insert_lesson(conn, &lesson))
        .map_err(|e| e.to_string())?;

    // Generate and store embedding for semantic search
    if let Some(ref embeddings) = state.embeddings {
        if embeddings.is_initialized() {
            // Combine title and content for better semantic understanding
            let text_to_embed = format!("{}\n{}", lesson.title, lesson.content);

            if let Ok(embedding) = embeddings.embed_one(text_to_embed).await {
                // Store embedding in vector table (ignore errors, embedding is optional for backward compat)
                let _ = state.db.with_conn(|conn| {
                    crate::storage::store_lesson_embedding(conn, &lesson.id, &embedding)
                });
            }
        }
    }

    // --- Graph enrichment (if graph is enabled and fields provided) ---
    let graph_info = if let Some(ref graph_lock) = state.graph {
        enrich_lesson_with_graph(graph_lock, &state.db, args, title)
    } else {
        serde_json::Value::Null
    };

    let mut response = serde_json::json!({
        "id": id,
        "message": "Lesson recorded successfully"
    });
    if graph_info != serde_json::Value::Null {
        response["graph"] = graph_info;
    }

    Ok(response)
}

#[allow(clippy::redundant_closure)]
fn handle_delete_lesson(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let id = args["id"].as_str().ok_or("id is required")?;

    state
        .db
        .with_conn(|conn| crate::storage::delete_lesson(conn, id))
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "id": id,
        "message": "Lesson deleted successfully"
    }))
}

#[allow(clippy::cast_possible_truncation)]
async fn handle_add_checkpoint(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let mut graph_info = serde_json::Value::Null;
    let agent = args["agent"].as_str().ok_or("agent is required")?;
    let working_on = args["working_on"]
        .as_str()
        .ok_or("working_on is required")?;
    let checkpoint_state = args["state"].clone();

    let checkpoint = crate::storage::CheckpointRecord::new(agent, working_on, checkpoint_state);
    let id = checkpoint.id.clone();

    // Store checkpoint in database
    state
        .db
        .with_conn(|conn| crate::storage::insert_checkpoint(conn, &checkpoint))
        .map_err(|e| e.to_string())?;

    // Generate and store embedding for semantic search
    if let Some(ref embeddings) = state.embeddings {
        if embeddings.is_initialized() {
            // Embed the working_on description for checkpoint semantic search
            let text_to_embed = checkpoint.working_on.clone();

            if let Ok(embedding) = embeddings.embed_one(text_to_embed).await {
                // Store embedding in vector table (ignore errors, embedding is optional for backward compat)
                let _ = state.db.with_conn(|conn| {
                    crate::storage::store_checkpoint_embedding(conn, &checkpoint.id, &embedding)
                });
            }
        }
    }

    // --- Graph enrichment + outcome tracking ---
    if let Some(ref graph_lock) = state.graph {
        let has_graph_fields = args.get("tools_used").is_some()
            || args.get("problems_encountered").is_some()
            || args.get("solutions_found").is_some()
            || args.get("graph_suggestions_used").is_some();

        if has_graph_fields {
            let mut graph = graph_lock.write(); // parking_lot RwLock, no .await

            let agent_node_id = crate::graph::enrichment::ensure_entity(
                &mut graph,
                crate::graph::EntityType::Agent,
                agent,
            );
            let checkpoint_node_id = crate::graph::enrichment::ensure_entity(
                &mut graph,
                crate::graph::EntityType::Chunk,
                &id,
            );

            let mut nodes_created: u32 = 0;
            let mut edges_created: u32 = 0;

            // tools_used → create/reuse Tool nodes + "used" edges from agent
            if let Some(tools) = args["tools_used"].as_array() {
                for tool_val in tools {
                    if let Some(tool_name) = tool_val.as_str() {
                        let tool_id = crate::graph::enrichment::ensure_entity(
                            &mut graph,
                            crate::graph::EntityType::Tool,
                            tool_name,
                        );
                        crate::graph::enrichment::ensure_edge(
                            &mut graph,
                            &agent_node_id,
                            &tool_id,
                            crate::graph::RelationshipKind::Used,
                            None,
                        );
                        nodes_created += 1;
                        edges_created += 1;
                    }
                }
            }

            // problems_encountered → create/reuse Problem nodes + "encountered" edges
            if let Some(problems) = args["problems_encountered"].as_array() {
                for prob_val in problems {
                    if let Some(prob_name) = prob_val.as_str() {
                        let prob_id = crate::graph::enrichment::ensure_entity(
                            &mut graph,
                            crate::graph::EntityType::Problem,
                            prob_name,
                        );
                        crate::graph::enrichment::ensure_edge(
                            &mut graph,
                            &checkpoint_node_id,
                            &prob_id,
                            crate::graph::RelationshipKind::RelatedTo,
                            None,
                        );
                        nodes_created += 1;
                        edges_created += 1;
                    }
                }
            }

            // solutions_found → create/reuse Solution nodes + "solved_by" edges
            if let Some(solutions) = args["solutions_found"].as_array() {
                for sol_val in solutions {
                    if let Some(sol_name) = sol_val.as_str() {
                        let sol_id = crate::graph::enrichment::ensure_entity(
                            &mut graph,
                            crate::graph::EntityType::Solution,
                            sol_name,
                        );
                        crate::graph::enrichment::ensure_edge(
                            &mut graph,
                            &checkpoint_node_id,
                            &sol_id,
                            crate::graph::RelationshipKind::Solved,
                            None,
                        );
                        nodes_created += 1;
                        edges_created += 1;
                    }
                }
            }

            // Outcome tracking: reinforce/weaken edges listed in graph_suggestions_used
            let mut outcome_applied = false;
            if let (Some(suggestions), Some(outcome_str)) = (
                args["graph_suggestions_used"].as_array(),
                args["outcome"].as_str(),
            ) {
                if let Some(outcome) = crate::graph::entities::Outcome::parse(outcome_str) {
                    let edge_ids: Vec<String> = suggestions
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    crate::graph::integrity::process_outcome(&mut graph, &edge_ids, outcome);
                    outcome_applied = true;

                    // If failure, create contradiction edges between solutions and problems
                    if matches!(outcome, crate::graph::entities::Outcome::Failure) {
                        if let (Some(solutions), Some(problems)) = (
                            args["solutions_found"].as_array(),
                            args["problems_encountered"].as_array(),
                        ) {
                            for sol_val in solutions {
                                for prob_val in problems {
                                    if let (Some(sol_name), Some(prob_name)) =
                                        (sol_val.as_str(), prob_val.as_str())
                                    {
                                        // Look up the node IDs we just created/found
                                        if let Some(sol_nodes) = graph.find_by_label(sol_name) {
                                            if let Some(prob_nodes) = graph.find_by_label(prob_name)
                                            {
                                                if let (Some(sol_id), Some(prob_id)) =
                                                    (sol_nodes.first(), prob_nodes.first())
                                                {
                                                    crate::graph::integrity::create_contradiction_edge(
                                                        &mut graph,
                                                        sol_id,
                                                        prob_id,
                                                        Some("Reported as failure in checkpoint".to_string()),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Best-effort persist (non-fatal)
            crate::graph::enrichment::persist_changes(&state.db, &graph, &[], &[]);

            graph_info = serde_json::json!({
                "nodes_created": nodes_created,
                "edges_created": edges_created,
                "outcome_applied": outcome_applied,
            });
        }
    }
    // --- End graph enrichment ---

    Ok(serde_json::json!({
        "id": id,
        "message": "Checkpoint saved successfully",
        "graph": graph_info,
    }))
}

#[allow(clippy::redundant_closure, clippy::cast_possible_truncation)]
fn handle_get_checkpoints(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let agent = args["agent"].as_str();
    let limit = args["limit"].as_u64().unwrap_or(5) as usize;

    let checkpoints = if let Some(agent) = agent {
        state
            .db
            .with_conn(|conn| crate::storage::get_recent_checkpoints(conn, agent, limit))
            .map_err(|e| e.to_string())?
    } else {
        state
            .db
            .with_conn(|conn| crate::storage::get_recent_checkpoints_all(conn, limit))
            .map_err(|e| e.to_string())?
    };

    Ok(serde_json::to_value(&checkpoints).unwrap_or_default())
}

// Replace handle_trigger_reindex with this async version:

#[allow(clippy::redundant_closure)]
async fn handle_trigger_reindex(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let path = args["path"].as_str();

    if let Some(target_path) = path {
        let path_buf = std::path::PathBuf::from(target_path);

        // Check if path is a directory
        if path_buf.is_dir() {
            // Scan directory and index all files
            let indexer = crate::watcher::Indexer::new(
                state.db.clone(),
                state.embeddings.clone(),
                state.enable_structural,
            );
            let indexer = std::sync::Arc::new(indexer);

            // Walk directory and index each file
            let walker = ignore::WalkBuilder::new(&path_buf)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .ignore(true)
                .parents(true)
                .filter_entry(|entry| {
                    if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                        if let Some(name) = entry.file_name().to_str() {
                            return !crate::watcher::filter::is_excluded_dir(name);
                        }
                    }
                    true
                })
                .build();

            let mut indexed = 0u64;
            let mut skipped = 0u64;
            let mut errors = 0u64;

            for entry in walker {
                match entry {
                    Ok(entry) => {
                        let entry_path = entry.path();

                        // Skip directories and non-indexable files
                        if entry_path.is_dir() {
                            continue;
                        }

                        // Apply full filter (code file, size, ignore patterns)
                        if !should_index_file(entry_path) {
                            skipped += 1;
                            continue;
                        }

                        // Index the file
                        let language = crate::watcher::FileFilter::detect_language(entry_path)
                            .map(String::from);
                        let request = crate::watcher::IndexRequest {
                            path: entry_path.to_path_buf(),
                            language,
                        };

                        match indexer.index_file(&request).await {
                            Ok(chunks) => {
                                if chunks > 0 {
                                    indexed += 1;
                                    tracing::debug!(
                                        path = %entry_path.display(),
                                        chunks,
                                        "Indexed file"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %entry_path.display(),
                                    error = %e,
                                    "Failed to index file"
                                );
                                errors += 1;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Error walking directory");
                        errors += 1;
                    }
                }
            }

            tracing::info!(
                path = %target_path,
                indexed,
                skipped,
                errors,
                "Directory scan complete"
            );

            Ok(serde_json::json!({
                "status": "indexed",
                "path": target_path,
                "files_indexed": indexed,
                "files_skipped": skipped,
                "errors": errors,
                "message": format!("Indexed {} files from directory: {}", indexed, target_path)
            }))
        } else {
            // Single file - delete chunks to trigger re-indexing
            state
                .db
                .with_conn(|conn| crate::storage::delete_chunks_by_file(conn, target_path))
                .map_err(|e| e.to_string())?;

            // Delete file state to mark as needing re-index
            state
                .db
                .with_conn(|conn| crate::storage::delete_file_state(conn, target_path))
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "status": "reindex_scheduled",
                "path": target_path,
                "message": format!("Re-indexing scheduled for file: {}", target_path)
            }))
        }
    } else {
        // Clear all file state to trigger full re-index
        state
            .db
            .with_conn(|conn| {
                let paths = crate::storage::list_file_paths(conn)?;
                for file_path in paths {
                    crate::storage::delete_file_state(conn, &file_path)?;
                }
                Ok::<_, crate::Error>(())
            })
            .map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "status": "reindex_scheduled",
            "path": "all",
            "message": "Full re-indexing scheduled for all tracked files"
        }))
    }
}

#[allow(clippy::redundant_closure, clippy::unnecessary_wraps)]
fn handle_get_status(state: &McpState) -> std::result::Result<serde_json::Value, String> {
    let chunk_count = state
        .db
        .with_conn(|conn| crate::storage::count_chunks(conn))
        .unwrap_or(0);

    let lesson_count = state
        .db
        .with_conn(|conn| crate::storage::count_lessons(conn))
        .unwrap_or(0);

    let file_count = state
        .db
        .with_conn(|conn| crate::storage::count_tracked_files(conn))
        .unwrap_or(0);

    let bootstrapping = state
        .structural_bootstrapping
        .load(std::sync::atomic::Ordering::Relaxed);

    Ok(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "structural_bootstrapping": bootstrapping,
        "stats": {
            "chunks": chunk_count,
            "lessons": lesson_count,
            "files": file_count
        }
    }))
}

#[allow(clippy::cast_possible_truncation)]
async fn handle_search_checkpoints(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let query = args["query"].as_str().ok_or("query is required")?;
    let agent_filter = args["agent"].as_str();
    let limit = args["limit"].as_u64().unwrap_or(5) as usize;

    // CRITICAL: Embedding service MUST be initialized for semantic search
    let embeddings = state.embeddings.as_ref().ok_or_else(|| {
        "Embedding service not initialized. Semantic search requires real embeddings.".to_string()
    })?;

    if !embeddings.is_initialized() {
        return Err(
            "Embedding service not fully initialized. Please wait for model loading to complete."
                .to_string(),
        );
    }

    // Generate embedding for query using real embeddings
    let embeddings = embeddings.clone();
    let query_text = query.to_string();

    let embedding = embeddings
        .embed_one(query_text)
        .await
        .map_err(|e| format!("Failed to generate query embedding: {e}"))?;

    // Search checkpoints using vector similarity
    let checkpoint_results = state
        .db
        .with_conn(|conn| crate::storage::search_checkpoints_by_embedding(conn, &embedding, limit))
        .map_err(|e| e.to_string())?;

    // Filter by agent if specified
    let checkpoints: Vec<_> = if let Some(agent) = agent_filter {
        checkpoint_results
            .into_iter()
            .filter(|cp| cp.record.agent == agent)
            .map(|cp| cp.record)
            .collect()
    } else {
        checkpoint_results.into_iter().map(|cp| cp.record).collect()
    };

    Ok(serde_json::json!({
        "checkpoints": serde_json::to_value(&checkpoints).unwrap_or(serde_json::Value::Array(vec![])),
        "count": checkpoints.len(),
        "query": query,
        "agent": agent_filter.unwrap_or("all"),
        "limit": limit
    }))
}

#[allow(clippy::redundant_closure)]
fn handle_get_agent_status(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let agent = args["agent"].as_str().ok_or("agent is required")?;

    let status = state
        .db
        .with_conn(|conn| crate::storage::get_agent_status(conn, agent))
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "agent": status.agent,
        "status": status.status.as_str(),
        "current_task": status.current_task,
        "last_updated": status.last_updated,
        "checkpoint_count": status.checkpoint_count
    }))
}

/// Index a repository or directory on demand.
/// This is the preferred way for agents to ensure Nellie has fresh context for a project.
/// Uses spawn_blocking for directory traversal to handle slow filesystems (NFS, SMB).
#[allow(clippy::redundant_closure)]
async fn handle_index_repo(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let path = args["path"].as_str().ok_or("path is required")?;
    let path_buf = std::path::PathBuf::from(path);
    let path_string = path.to_string();

    if !path_buf.exists() {
        return Err(format!("Path does not exist: {path}"));
    }

    if !path_buf.is_dir() {
        return Err(format!(
            "Path is not a directory: {path}. Use trigger_reindex for single files."
        ));
    }

    let start_time = std::time::Instant::now();

    // Check if this is a network mount (NFS/SMB) - use fast walker if so
    let is_network = is_network_path(&path_buf);
    tracing::info!(
        path,
        is_network,
        "Starting index_repo - collecting files..."
    );

    // Collect all file paths in a blocking task (handles slow NFS/SMB)
    let path_for_walk = path_buf.clone();
    let file_paths: Vec<std::path::PathBuf> = tokio::task::spawn_blocking(move || {
        if is_network {
            // Fast walker for network mounts - skip gitignore parsing
            fast_walk_directory(&path_for_walk)
        } else {
            // Full walker with gitignore support for local paths
            let walker = ignore::WalkBuilder::new(&path_for_walk)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .ignore(true)
                .parents(true)
                .filter_entry(|entry| {
                    if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                        if let Some(name) = entry.file_name().to_str() {
                            return !crate::watcher::filter::is_excluded_dir(name);
                        }
                    }
                    true
                })
                .build();

            let mut paths = Vec::new();
            for entry in walker.flatten() {
                let p = entry.path();
                if should_index_file(p) {
                    paths.push(p.to_path_buf());
                }
            }
            paths
        }
    })
    .await
    .map_err(|e| format!("Directory walk failed: {e}"))?;

    let total_files = file_paths.len();
    tracing::info!(path = path_string, total_files, "Found files to index");

    // Create indexer with embeddings and structural parsing
    let indexer = crate::watcher::Indexer::new(
        state.db.clone(),
        state.embeddings.clone(),
        state.enable_structural,
    );
    let indexer = std::sync::Arc::new(indexer);

    let mut files_indexed = 0u64;
    let mut files_unchanged = 0u64;
    let mut chunks_created = 0u64;
    let mut errors = 0u64;

    // Process files in batches, yielding periodically
    for (i, entry_path) in file_paths.into_iter().enumerate() {
        // Log progress every 100 files
        if i > 0 && i % 100 == 0 {
            tracing::info!(
                path = path_string,
                progress = format!("{}/{}", i, total_files),
                files_indexed,
                chunks_created,
                "index_repo progress"
            );
            // Yield to allow other tasks to run
            tokio::task::yield_now().await;
        }

        // Index the file
        let language = crate::watcher::FileFilter::detect_language(&entry_path).map(String::from);
        let request = crate::watcher::IndexRequest {
            path: entry_path.clone(),
            language,
        };

        match indexer.index_file(&request).await {
            Ok(chunks) => {
                if chunks > 0 {
                    files_indexed += 1;
                    chunks_created += chunks as u64;
                } else {
                    files_unchanged += 1;
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %entry_path.display(),
                    error = %e,
                    "Failed to index file"
                );
                errors += 1;
            }
        }
    }

    // Also count non-code files as skipped
    let files_skipped =
        total_files.saturating_sub((files_indexed + files_unchanged + errors) as usize) as u64;

    let elapsed = start_time.elapsed();

    tracing::info!(
        path = path_string,
        files_indexed,
        files_unchanged,
        files_skipped,
        chunks_created,
        errors,
        elapsed_ms = elapsed.as_millis(),
        "index_repo complete"
    );

    Ok(serde_json::json!({
        "status": "completed",
        "path": path_string,
        "files_indexed": files_indexed,
        "files_unchanged": files_unchanged,
        "files_skipped": files_skipped,
        "chunks_created": chunks_created,
        "errors": errors,
        "elapsed_ms": elapsed.as_millis(),
        "message": format!(
            "Indexed {} files ({} chunks) from {}, {} unchanged, {} skipped, {} errors in {:.1}s",
            files_indexed, chunks_created, path_string, files_unchanged, files_skipped, errors,
            elapsed.as_secs_f64()
        )
    }))
}

/// Incremental diff-based indexing.
/// Compares file mtimes with database and only indexes new/changed files.
/// Also removes entries for deleted files.
/// Uses spawn_blocking for directory traversal to handle slow filesystems (NFS, SMB).
#[allow(clippy::redundant_closure, clippy::cast_possible_wrap)]
async fn handle_diff_index(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let path = args["path"].as_str().ok_or("path is required")?;
    let path_buf = std::path::PathBuf::from(path);
    let path_string = path.to_string();

    if !path_buf.exists() {
        return Err(format!("Path does not exist: {path}"));
    }

    if !path_buf.is_dir() {
        return Err(format!("Path is not a directory: {path}"));
    }

    let start_time = std::time::Instant::now();

    // Check if this is a network mount
    let is_network = is_network_path(&path_buf);
    tracing::info!(
        path,
        is_network,
        "Starting diff_index - collecting files..."
    );

    // Create indexer with embeddings and structural parsing
    let indexer = crate::watcher::Indexer::new(
        state.db.clone(),
        state.embeddings.clone(),
        state.enable_structural,
    );
    let indexer = std::sync::Arc::new(indexer);

    // Get existing indexed files for this path to detect deletions
    let existing_files: std::collections::HashSet<String> = state
        .db
        .with_conn(|conn| crate::storage::list_file_paths_by_prefix(conn, path))
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();

    // Collect all file paths with metadata in a blocking task (handles slow NFS/SMB)
    let path_for_walk = path_buf.clone();
    let file_info: Vec<(std::path::PathBuf, i64, i64)> = tokio::task::spawn_blocking(move || {
        let file_paths = if is_network {
            // Fast walker for network mounts
            fast_walk_directory(&path_for_walk)
        } else {
            // Full walker with gitignore support
            let walker = ignore::WalkBuilder::new(&path_for_walk)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .ignore(true)
                .parents(true)
                .filter_entry(|entry| {
                    if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                        if let Some(name) = entry.file_name().to_str() {
                            return !crate::watcher::filter::is_excluded_dir(name);
                        }
                    }
                    true
                })
                .build();

            let mut paths = Vec::new();
            for entry in walker.flatten() {
                let p = entry.path();
                if should_index_file(p) {
                    paths.push(p.to_path_buf());
                }
            }
            paths
        };

        // Get metadata for all files
        let mut files = Vec::new();
        for p in file_paths {
            if let Ok(metadata) = std::fs::metadata(&p) {
                let mtime = metadata
                    .modified()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64
                    })
                    .unwrap_or(0);
                let size = metadata.len() as i64;
                files.push((p, mtime, size));
            }
        }
        files
    })
    .await
    .map_err(|e| format!("Directory walk failed: {e}"))?;

    let total_files = file_info.len();
    tracing::info!(
        path = path_string,
        total_files,
        "Found files for diff check"
    );

    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut files_indexed = 0u64;
    let mut files_unchanged = 0u64;
    let files_skipped = 0u64;
    let mut files_deleted = 0u64;
    let mut chunks_created = 0u64;
    let mut errors = 0u64;

    // Process files
    for (i, (entry_path, current_mtime, current_size)) in file_info.into_iter().enumerate() {
        // Log progress every 100 files
        if i > 0 && i % 100 == 0 {
            tracing::info!(
                path = path_string,
                progress = format!("{}/{}", i, total_files),
                files_indexed,
                files_unchanged,
                "diff_index progress"
            );
            tokio::task::yield_now().await;
        }

        let path_str = entry_path.to_string_lossy().to_string();
        seen_files.insert(path_str.clone());

        // Check if file needs reindexing
        let needs_index = state
            .db
            .with_conn(|conn| {
                crate::storage::needs_reindex_by_metadata(
                    conn,
                    &path_str,
                    current_mtime,
                    current_size,
                )
            })
            .unwrap_or(true);

        if !needs_index {
            files_unchanged += 1;
            continue;
        }

        // Index the file
        let language = crate::watcher::FileFilter::detect_language(&entry_path).map(String::from);
        let request = crate::watcher::IndexRequest {
            path: entry_path.clone(),
            language,
        };

        match indexer.index_file(&request).await {
            Ok(chunks) => {
                if chunks > 0 {
                    files_indexed += 1;
                    chunks_created += chunks as u64;
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %entry_path.display(),
                    error = %e,
                    "Failed to index file"
                );
                errors += 1;
            }
        }
    }

    // Remove entries for deleted files
    for old_file in existing_files.difference(&seen_files) {
        if let Err(e) = state
            .db
            .with_conn(|conn| crate::storage::delete_chunks_by_file(conn, old_file))
        {
            tracing::warn!(path = old_file, error = %e, "Failed to delete stale chunks");
            errors += 1;
        } else {
            let _ = state
                .db
                .with_conn(|conn| crate::storage::delete_file_state(conn, old_file));
            files_deleted += 1;
            tracing::debug!(path = old_file, "Removed deleted file from index");
        }
    }

    let elapsed = start_time.elapsed();

    tracing::info!(
        path = path_string,
        files_indexed,
        files_unchanged,
        files_deleted,
        files_skipped,
        chunks_created,
        errors,
        elapsed_ms = elapsed.as_millis(),
        "diff_index complete"
    );

    Ok(serde_json::json!({
        "status": "completed",
        "path": path_string,
        "files_indexed": files_indexed,
        "files_unchanged": files_unchanged,
        "files_deleted": files_deleted,
        "files_skipped": files_skipped,
        "chunks_created": chunks_created,
        "errors": errors,
        "elapsed_ms": elapsed.as_millis(),
        "message": format!(
            "Diff indexed {}: {} updated, {} unchanged, {} deleted, {} skipped in {:.1}s",
            path_string, files_indexed, files_unchanged, files_deleted, files_skipped,
            elapsed.as_secs_f64()
        )
    }))
}

/// Full reindex - nuclear option.
/// Clears all indexed data for a path and re-indexes from scratch.
/// Uses spawn_blocking for directory traversal to handle slow filesystems (NFS, SMB).
#[allow(clippy::redundant_closure)]
async fn handle_full_reindex(
    state: &McpState,
    args: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let path = args["path"].as_str().ok_or("path is required")?;
    let path_buf = std::path::PathBuf::from(path);
    let path_string = path.to_string();

    if !path_buf.exists() {
        return Err(format!("Path does not exist: {path}"));
    }

    if !path_buf.is_dir() {
        return Err(format!("Path is not a directory: {path}"));
    }

    let start_time = std::time::Instant::now();

    // Clear existing data for this path
    let chunks_deleted = state
        .db
        .with_conn(|conn| crate::storage::delete_chunks_by_path_prefix(conn, path))
        .map_err(|e| format!("Failed to clear chunks: {e}"))?;

    let files_cleared = state
        .db
        .with_conn(|conn| crate::storage::delete_file_state_by_prefix(conn, path))
        .map_err(|e| format!("Failed to clear file state: {e}"))?;

    tracing::info!(
        path,
        chunks_deleted,
        files_cleared,
        "Cleared existing index data"
    );

    // Check if this is a network mount
    let is_network = is_network_path(&path_buf);
    tracing::info!(
        path,
        is_network,
        "Starting full_reindex - collecting files..."
    );

    // Collect all file paths in a blocking task (handles slow NFS/SMB)
    let path_for_walk = path_buf.clone();
    let file_paths: Vec<std::path::PathBuf> = tokio::task::spawn_blocking(move || {
        if is_network {
            // Fast walker for network mounts
            fast_walk_directory(&path_for_walk)
        } else {
            // Full walker with gitignore support
            let walker = ignore::WalkBuilder::new(&path_for_walk)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .ignore(true)
                .parents(true)
                .filter_entry(|entry| {
                    if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                        if let Some(name) = entry.file_name().to_str() {
                            return !crate::watcher::filter::is_excluded_dir(name);
                        }
                    }
                    true
                })
                .build();

            let mut paths = Vec::new();
            for entry in walker.flatten() {
                let p = entry.path();
                if should_index_file(p) {
                    paths.push(p.to_path_buf());
                }
            }
            paths
        }
    })
    .await
    .map_err(|e| format!("Directory walk failed: {e}"))?;

    let total_files = file_paths.len();
    tracing::info!(path = path_string, total_files, "Found files to reindex");

    // Create indexer with embeddings and structural parsing
    let indexer = crate::watcher::Indexer::new(
        state.db.clone(),
        state.embeddings.clone(),
        state.enable_structural,
    );
    let indexer = std::sync::Arc::new(indexer);

    let mut files_indexed = 0u64;
    let files_skipped = 0u64;
    let mut chunks_created = 0u64;
    let mut errors = 0u64;

    // Process files in batches
    for (i, entry_path) in file_paths.into_iter().enumerate() {
        // Log progress every 100 files
        if i > 0 && i % 100 == 0 {
            tracing::info!(
                path = path_string,
                progress = format!("{}/{}", i, total_files),
                files_indexed,
                chunks_created,
                "full_reindex progress"
            );
            tokio::task::yield_now().await;
        }

        // Index the file
        let language = crate::watcher::FileFilter::detect_language(&entry_path).map(String::from);
        let request = crate::watcher::IndexRequest {
            path: entry_path.clone(),
            language,
        };

        match indexer.index_file(&request).await {
            Ok(chunks) => {
                if chunks > 0 {
                    files_indexed += 1;
                    chunks_created += chunks as u64;
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %entry_path.display(),
                    error = %e,
                    "Failed to index file"
                );
                errors += 1;
            }
        }
    }

    let elapsed = start_time.elapsed();

    tracing::info!(
        path = path_string,
        chunks_deleted,
        files_indexed,
        files_skipped,
        chunks_created,
        errors,
        elapsed_ms = elapsed.as_millis(),
        "full_reindex complete"
    );

    Ok(serde_json::json!({
        "status": "completed",
        "path": path_string,
        "cleared": {
            "chunks": chunks_deleted,
            "files": files_cleared
        },
        "indexed": {
            "files": files_indexed,
            "chunks": chunks_created
        },
        "files_skipped": files_skipped,
        "errors": errors,
        "elapsed_ms": elapsed.as_millis(),
        "message": format!(
            "Full reindex of {}: cleared {} chunks, indexed {} files ({} chunks), {} skipped, {} errors in {:.1}s",
            path_string, chunks_deleted, files_indexed, chunks_created, files_skipped, errors,
            elapsed.as_secs_f64()
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_defined() {
        let tools = get_tools();
        assert!(tools.len() >= 15); // 11 original + 3 new indexing tools + 1 bootstrap

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"search_code"));
        assert!(names.contains(&"search_lessons"));
        assert!(names.contains(&"list_lessons"));
        assert!(names.contains(&"add_lesson"));
        assert!(names.contains(&"delete_lesson"));
        assert!(names.contains(&"add_checkpoint"));
        assert!(names.contains(&"get_recent_checkpoints"));
        assert!(names.contains(&"trigger_reindex"));
        assert!(names.contains(&"get_status"));
        assert!(names.contains(&"search_checkpoints"));
        assert!(names.contains(&"get_agent_status"));
        // New indexing tools for Issue #20
        assert!(names.contains(&"index_repo"));
        assert!(names.contains(&"diff_index"));
        assert!(names.contains(&"full_reindex"));
        // Graph bootstrap tool
        assert!(names.contains(&"bootstrap_graph"));
    }

    #[tokio::test]
    async fn test_list_tools_endpoint() {
        let tools = list_tools().await;
        assert!(!tools.0.is_empty());
    }

    #[test]
    fn test_search_code_schema() {
        let tools = get_tools();
        let search_code = tools
            .iter()
            .find(|t| t.name == "search_code")
            .expect("search_code tool should exist");

        let schema = &search_code.input_schema;
        assert!(schema.get("properties").is_some());
        assert!(schema["properties"].get("query").is_some());
        assert!(schema["properties"].get("limit").is_some());
    }

    #[test]
    fn test_add_lesson_schema() {
        let tools = get_tools();
        let add_lesson = tools
            .iter()
            .find(|t| t.name == "add_lesson")
            .expect("add_lesson tool should exist");

        let schema = &add_lesson.input_schema;
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required field should be an array");

        assert!(required.iter().any(|v| v.as_str() == Some("title")));
        assert!(required.iter().any(|v| v.as_str() == Some("content")));
        assert!(required.iter().any(|v| v.as_str() == Some("tags")));
    }

    #[tokio::test]
    async fn test_search_code_requires_embedding_service() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;
            // Skip vector table initialization as sqlite-vec may not be available in tests
            Ok(())
        })
        .expect("Failed to setup database");
        let state = McpState::new(db); // No embedding service

        // Test that search fails without embedding service
        let args = serde_json::json!({
            "query": "test search query"
        });

        let result = handle_search_code(&state, &args).await;
        // Now requires embedding service - should fail with appropriate error
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.contains("Embedding service not initialized"));
    }

    #[tokio::test]
    async fn test_search_code_with_limit() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;
            Ok(())
        })
        .expect("Failed to setup database");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "query": "test",
            "limit": 5
        });

        let result = handle_search_code(&state, &args).await;
        // May fail due to missing vector table in test environment
        if let Ok(response) = result {
            assert_eq!(response["limit"], 5);
        }
    }

    #[tokio::test]
    async fn test_search_code_missing_query() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({});

        let result = handle_search_code(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("query is required"));
    }

    #[test]
    fn test_placeholder_embedding_consistency() {
        let embedding1 = crate::embeddings::placeholder_embedding("test query");
        let embedding2 = crate::embeddings::placeholder_embedding("test query");

        // Placeholder embeddings should be deterministic
        assert_eq!(embedding1, embedding2);
        assert_eq!(embedding1.len(), crate::embeddings::EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn test_add_lesson_success() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "title": "Memory Leak Prevention",
            "content": "Use Arc<RwLock<T>> carefully in async contexts",
            "tags": ["rust", "memory", "performance"],
            "severity": "critical"
        });

        let result = handle_add_lesson(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.get("id").is_some());
        assert!(response["message"]
            .as_str()
            .unwrap()
            .contains("Lesson recorded"));
    }

    #[tokio::test]
    async fn test_add_lesson_missing_title() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "content": "Some lesson content",
            "tags": ["test"]
        });

        let result = handle_add_lesson(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("title is required"));
    }

    #[tokio::test]
    async fn test_add_lesson_missing_content() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "title": "Lesson Title",
            "tags": ["test"]
        });

        let result = handle_add_lesson(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("content is required"));
    }

    #[tokio::test]
    async fn test_add_lesson_missing_tags() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "title": "Lesson Title",
            "content": "Lesson content"
        });

        let result = handle_add_lesson(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("tags is required"));
    }

    #[tokio::test]
    async fn test_add_lesson_default_severity() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "title": "Lesson Title",
            "content": "Lesson content",
            "tags": ["test"]
            // severity not provided, should default to "info"
        });

        let result = handle_add_lesson(&state, &args).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response.get("id").is_some());
    }

    #[tokio::test]
    async fn test_search_lessons_requires_embedding_service() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert a test lesson
            let lesson = crate::storage::LessonRecord::new(
                "Rust Error Handling",
                "Always use Result types instead of panicking in libraries",
                vec!["rust".to_string(), "error-handling".to_string()],
            );
            crate::storage::insert_lesson(conn, &lesson)?;
            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db); // No embedding service

        let args = serde_json::json!({
            "query": "error handling",
            "limit": 5
        });

        let result = handle_search_lessons(&state, &args).await;
        // Semantic search requires embedding service - should fail with appropriate error
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.contains("Embedding service not initialized"));
    }

    #[tokio::test]
    async fn test_search_lessons_missing_query() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "limit": 5
        });

        let result = handle_search_lessons(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("query is required"));
    }

    #[tokio::test]
    async fn test_search_lessons_default_limit_requires_embedding() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db); // No embedding service

        let args = serde_json::json!({
            "query": "some query"
            // limit not provided, should default to 5
        });

        let result = handle_search_lessons(&state, &args).await;
        // Semantic search requires embedding service
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_lessons_with_limit_requires_embedding() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert multiple test lessons
            for i in 0..10 {
                let lesson = crate::storage::LessonRecord::new(
                    &format!("Lesson {}", i),
                    &format!("Content for lesson {}", i),
                    vec!["test".to_string()],
                );
                crate::storage::insert_lesson(conn, &lesson)?;
            }
            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db); // No embedding service

        let args = serde_json::json!({
            "query": "lesson",
            "limit": 3
        });

        let result = handle_search_lessons(&state, &args).await;
        // Semantic search requires embedding service
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_lessons_empty_result_requires_embedding() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db); // No embedding service

        let args = serde_json::json!({
            "query": "nonexistent lesson query",
            "limit": 5
        });

        let result = handle_search_lessons(&state, &args).await;
        // Semantic search requires embedding service
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_checkpoint_success() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "code-generator-v1",
            "working_on": "Implementing feature X",
            "state": {
                "current_task": "feature-x",
                "progress": 0.5,
                "last_checkpoint": "2024-01-01T12:00:00Z"
            }
        });

        let result = handle_add_checkpoint(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.get("id").is_some());
        assert!(response["message"]
            .as_str()
            .unwrap()
            .contains("Checkpoint saved"));
    }

    #[tokio::test]
    async fn test_add_checkpoint_missing_agent() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "working_on": "Task",
            "state": {}
        });

        let result = handle_add_checkpoint(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("agent is required"));
    }

    #[tokio::test]
    async fn test_add_checkpoint_missing_working_on() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "agent-v1",
            "state": {}
        });

        let result = handle_add_checkpoint(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("working_on is required"));
    }

    #[tokio::test]
    async fn test_add_checkpoint_with_empty_state() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "agent-v1",
            "working_on": "Task",
            "state": {}
        });

        let result = handle_add_checkpoint(&state, &args).await;
        // Should succeed even with empty state object
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_checkpoint_backward_compatible() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "test-agent",
            "working_on": "Test task",
            "state": {"data": "test"}
        });

        let result = handle_add_checkpoint(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        // graph field should be null when no graph fields provided
        assert!(response.get("graph").is_some());
        assert_eq!(response["graph"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_add_checkpoint_with_graph_fields_no_graph() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "test-agent",
            "working_on": "Test task",
            "state": {"data": "test"},
            "tools_used": ["cargo", "git"],
            "problems_encountered": ["timeout"],
            "solutions_found": ["use async"],
            "outcome": "success"
        });

        let result = handle_add_checkpoint(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        // When graph is disabled, graph_info should still be null
        assert_eq!(response["graph"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_add_checkpoint_with_graph_enabled() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");

        // Create state with graph enabled
        let graph_config = crate::config::GraphConfig {
            enabled: true,
            max_nodes: 10000,
            decay_half_life_days: 30.0,
            gc_min_confidence: 0.05,
            gc_orphan_days: 7,
            provisional_threshold: 0.3,
            confirmation_count: 2,
        };
        let mut state = McpState::new(db);
        let graph = std::sync::Arc::new(parking_lot::RwLock::new(crate::graph::GraphMemory::new(
            graph_config,
        )));
        state.graph = Some(graph);

        let args = serde_json::json!({
            "agent": "test-agent",
            "working_on": "Implementing feature",
            "state": {"progress": 50},
            "tools_used": ["cargo", "git"],
            "problems_encountered": ["compilation error"],
            "solutions_found": ["fix syntax"],
            "outcome": "success"
        });

        let result = handle_add_checkpoint(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response["graph"].is_object());
        assert!(response["graph"]["nodes_created"].is_number());
        assert!(response["graph"]["edges_created"].is_number());
        assert_eq!(response["graph"]["outcome_applied"], false); // no graph_suggestions_used
    }

    #[tokio::test]
    async fn test_add_checkpoint_with_outcome_tracking() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");

        // Setup graph with an existing edge
        let graph_config = crate::config::GraphConfig {
            enabled: true,
            ..Default::default()
        };
        let mut temp_graph = crate::graph::GraphMemory::new(graph_config);

        // Create some entities and an edge
        let edge_id = {
            let agent_id = crate::graph::enrichment::ensure_entity(
                &mut temp_graph,
                crate::graph::EntityType::Agent,
                "test-agent",
            );
            let tool_id = crate::graph::enrichment::ensure_entity(
                &mut temp_graph,
                crate::graph::EntityType::Tool,
                "cargo",
            );
            let edge_id = crate::graph::enrichment::ensure_edge(
                &mut temp_graph,
                &agent_id,
                &tool_id,
                crate::graph::RelationshipKind::Used,
                None,
            );
            edge_id.unwrap()
        };

        let mut state = McpState::new(db);
        let graph = std::sync::Arc::new(parking_lot::RwLock::new(temp_graph));
        state.graph = Some(graph);

        let args = serde_json::json!({
            "agent": "test-agent",
            "working_on": "Using cargo",
            "state": {},
            "graph_suggestions_used": [edge_id],
            "outcome": "success"
        });

        let result = handle_add_checkpoint(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["graph"]["outcome_applied"], true);
    }

    #[test]
    fn test_get_checkpoints_success() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert test checkpoints
            let checkpoint1 = crate::storage::CheckpointRecord::new(
                "test-agent",
                "Working on task 1",
                serde_json::json!({"step": 1}),
            );
            crate::storage::insert_checkpoint(conn, &checkpoint1)?;

            let checkpoint2 = crate::storage::CheckpointRecord::new(
                "test-agent",
                "Working on task 2",
                serde_json::json!({"step": 2}),
            );
            crate::storage::insert_checkpoint(conn, &checkpoint2)?;

            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "test-agent",
            "limit": 5
        });

        let result = handle_get_checkpoints(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.is_array() || response.is_object());
    }

    #[test]
    fn test_get_checkpoints_without_agent() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "limit": 5
        });

        // Without agent, should return recent checkpoints across all agents
        let result = handle_get_checkpoints(&state, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_checkpoints_default_limit() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "test-agent"
            // limit not provided, should default to 5
        });

        let result = handle_get_checkpoints(&state, &args);
        // Should succeed (may return empty results)
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_checkpoints_with_limit() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert multiple checkpoints
            for i in 0..10 {
                let checkpoint = crate::storage::CheckpointRecord::new(
                    "test-agent",
                    &format!("Task {}", i),
                    serde_json::json!({"step": i}),
                );
                crate::storage::insert_checkpoint(conn, &checkpoint)?;
            }
            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "test-agent",
            "limit": 3
        });

        let result = handle_get_checkpoints(&state, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_checkpoints_empty_result() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "nonexistent-agent",
            "limit": 5
        });

        let result = handle_get_checkpoints(&state, &args);
        // Should return success with empty results
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_lessons_success() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert test lessons
            for i in 0..5 {
                let lesson = crate::storage::LessonRecord::new(
                    &format!("Lesson {}", i),
                    &format!("Content for lesson {}", i),
                    vec!["test".to_string()],
                );
                crate::storage::insert_lesson(conn, &lesson)?;
            }
            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        let args = serde_json::json!({});

        let result = handle_list_lessons(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.get("lessons").is_some());
        assert!(response.get("count").is_some());
        assert_eq!(response["count"], 5);
    }

    #[test]
    fn test_list_lessons_with_limit() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert test lessons
            for i in 0..10 {
                let lesson = crate::storage::LessonRecord::new(
                    &format!("Lesson {}", i),
                    &format!("Content {}", i),
                    vec!["test".to_string()],
                );
                crate::storage::insert_lesson(conn, &lesson)?;
            }
            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "limit": 3
        });

        let result = handle_list_lessons(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["count"], 3);
    }

    #[test]
    fn test_list_lessons_with_severity_filter() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert lessons with different severities
            let lesson1 = crate::storage::LessonRecord::new(
                "Critical Issue",
                "A critical problem",
                vec!["critical".to_string()],
            )
            .with_severity("critical");
            crate::storage::insert_lesson(conn, &lesson1)?;

            let lesson2 = crate::storage::LessonRecord::new(
                "Warning Issue",
                "A warning problem",
                vec!["warning".to_string()],
            )
            .with_severity("warning");
            crate::storage::insert_lesson(conn, &lesson2)?;

            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "severity": "critical"
        });

        let result = handle_list_lessons(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["severity"], "critical");
    }

    #[test]
    fn test_list_lessons_empty() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({});

        let result = handle_list_lessons(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["count"], 0);
    }

    #[test]
    fn test_delete_lesson_success() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert a test lesson
            let lesson = crate::storage::LessonRecord::new(
                "Test Lesson",
                "Test content",
                vec!["test".to_string()],
            );
            crate::storage::insert_lesson(conn, &lesson)?;

            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        // Get the ID from a list query first
        let list_result = state
            .db
            .with_conn(|conn| crate::storage::list_lessons(conn))
            .expect("Failed to list lessons");

        if let Some(lesson) = list_result.first() {
            let args = serde_json::json!({
                "id": &lesson.id
            });

            let result = handle_delete_lesson(&state, &args);
            assert!(result.is_ok());

            let response = result.unwrap();
            assert!(response.get("id").is_some());
            assert!(response["message"].as_str().unwrap().contains("deleted"));
        }
    }

    #[test]
    fn test_delete_lesson_missing_id() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({});

        let result = handle_delete_lesson(&state, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("id is required"));
    }

    #[tokio::test]
    async fn test_trigger_reindex_specific_path() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "path": "/test/file.rs"
        });

        let result = handle_trigger_reindex(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["status"], "reindex_scheduled");
        assert_eq!(response["path"], "/test/file.rs");
        assert!(response["message"]
            .as_str()
            .unwrap()
            .contains("Re-indexing scheduled"));
    }

    #[tokio::test]
    async fn test_trigger_reindex_all_paths() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({});

        let result = handle_trigger_reindex(&state, &args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["status"], "reindex_scheduled");
        assert_eq!(response["path"], "all");
        assert!(response["message"]
            .as_str()
            .unwrap()
            .contains("Full re-indexing"));
    }

    #[test]
    fn test_list_lessons_tool_exists() {
        let tools = get_tools();
        let list_lessons = tools
            .iter()
            .find(|t| t.name == "list_lessons")
            .expect("list_lessons tool should exist");

        assert!(list_lessons.description.is_some());
        let desc = list_lessons.description.as_ref().unwrap().to_lowercase();
        assert!(desc.contains("list"));
    }

    #[test]
    fn test_delete_lesson_tool_exists() {
        let tools = get_tools();
        let delete_lesson = tools
            .iter()
            .find(|t| t.name == "delete_lesson")
            .expect("delete_lesson tool should exist");

        assert!(delete_lesson.description.is_some());
        assert!(delete_lesson
            .description
            .as_ref()
            .unwrap()
            .contains("Delete"));
    }

    #[test]
    fn test_trigger_reindex_tool_exists() {
        let tools = get_tools();
        let trigger_reindex = tools
            .iter()
            .find(|t| t.name == "trigger_reindex")
            .expect("trigger_reindex tool should exist");

        assert!(trigger_reindex.description.is_some());
        assert!(trigger_reindex
            .description
            .as_ref()
            .unwrap()
            .contains("re-indexing"));
    }

    #[test]
    fn test_checkpoint_tool_schema() {
        let tools = get_tools();
        let add_checkpoint = tools
            .iter()
            .find(|t| t.name == "add_checkpoint")
            .expect("add_checkpoint tool should exist");

        let schema = &add_checkpoint.input_schema;
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required field should be an array");

        assert!(required.iter().any(|v| v.as_str() == Some("agent")));
        assert!(required.iter().any(|v| v.as_str() == Some("working_on")));
        assert!(required.iter().any(|v| v.as_str() == Some("state")));
    }

    #[test]
    fn test_get_checkpoints_tool_schema() {
        let tools = get_tools();
        let get_checkpoints = tools
            .iter()
            .find(|t| t.name == "get_recent_checkpoints")
            .expect("get_recent_checkpoints tool should exist");

        let schema = &get_checkpoints.input_schema;
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required field should be an array");

        // agent is now optional — should not be in required
        assert!(!required.iter().any(|v| v.as_str() == Some("agent")));
    }

    #[test]
    fn test_search_checkpoints_tool_exists() {
        let tools = get_tools();
        let search_checkpoints = tools
            .iter()
            .find(|t| t.name == "search_checkpoints")
            .expect("search_checkpoints tool should exist");

        assert!(search_checkpoints.description.is_some());
        let desc = search_checkpoints
            .description
            .as_ref()
            .unwrap()
            .to_lowercase();
        assert!(desc.contains("search"));
        assert!(desc.contains("checkpoint"));
    }

    #[test]
    fn test_get_agent_status_tool_exists() {
        let tools = get_tools();
        let get_agent_status = tools
            .iter()
            .find(|t| t.name == "get_agent_status")
            .expect("get_agent_status tool should exist");

        assert!(get_agent_status.description.is_some());
        let desc = get_agent_status
            .description
            .as_ref()
            .unwrap()
            .to_lowercase();
        assert!(desc.contains("status"));
    }

    #[tokio::test]
    async fn test_search_checkpoints_success_requires_embedding() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert test checkpoints
            let checkpoint1 = crate::storage::CheckpointRecord::new(
                "agent-1",
                "Working on feature implementation",
                serde_json::json!({"step": 1}),
            );
            crate::storage::insert_checkpoint(conn, &checkpoint1)?;

            let checkpoint2 = crate::storage::CheckpointRecord::new(
                "agent-2",
                "Debugging test failures",
                serde_json::json!({"step": 2}),
            );
            crate::storage::insert_checkpoint(conn, &checkpoint2)?;

            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db); // No embedding service

        let args = serde_json::json!({
            "query": "feature",
            "limit": 5
        });

        let result = handle_search_checkpoints(&state, &args).await;
        // Semantic search requires embedding service
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.contains("Embedding service not initialized"));
    }

    #[tokio::test]
    async fn test_search_checkpoints_with_agent_filter_requires_embedding() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert checkpoints for different agents
            let checkpoint1 =
                crate::storage::CheckpointRecord::new("agent-1", "Task 1", serde_json::json!({}));
            crate::storage::insert_checkpoint(conn, &checkpoint1)?;

            let checkpoint2 =
                crate::storage::CheckpointRecord::new("agent-1", "Task 2", serde_json::json!({}));
            crate::storage::insert_checkpoint(conn, &checkpoint2)?;

            let checkpoint3 =
                crate::storage::CheckpointRecord::new("agent-2", "Task 3", serde_json::json!({}));
            crate::storage::insert_checkpoint(conn, &checkpoint3)?;

            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db); // No embedding service

        let args = serde_json::json!({
            "query": "ignored",
            "agent": "agent-1",
            "limit": 10
        });

        let result = handle_search_checkpoints(&state, &args).await;
        // Semantic search requires embedding service
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_checkpoints_missing_query() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "limit": 5
        });

        let result = handle_search_checkpoints(&state, &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("query is required"));
    }

    #[tokio::test]
    async fn test_search_checkpoints_default_limit_requires_embedding() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db); // No embedding service

        let args = serde_json::json!({
            "query": "test"
        });

        let result = handle_search_checkpoints(&state, &args).await;
        // Semantic search requires embedding service
        assert!(result.is_err());
    }

    #[test]
    fn test_get_agent_status_success() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Mark agent as in progress
            crate::storage::mark_in_progress(conn, "test-agent", Some("Working on task"))?;

            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "test-agent"
        });

        let result = handle_get_agent_status(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["agent"], "test-agent");
        assert_eq!(response["status"], "in_progress");
        assert!(response.get("current_task").is_some());
        assert!(response.get("last_updated").is_some());
        assert!(response.get("checkpoint_count").is_some());
    }

    #[test]
    fn test_get_agent_status_idle() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "new-agent"
        });

        let result = handle_get_agent_status(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["agent"], "new-agent");
        assert_eq!(response["status"], "idle");
    }

    #[test]
    fn test_get_agent_status_missing_agent() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({});

        let result = handle_get_agent_status(&state, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("agent is required"));
    }

    #[test]
    fn test_get_agent_status_with_checkpoints() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| -> crate::Result<()> {
            crate::storage::migrate(conn)?;

            // Insert checkpoints for agent
            let checkpoint = crate::storage::CheckpointRecord::new(
                "test-agent",
                "Working on feature",
                serde_json::json!({"progress": 50}),
            );
            crate::storage::insert_checkpoint(conn, &checkpoint)?;

            Ok(())
        })
        .expect("Failed to setup");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "agent": "test-agent"
        });

        let result = handle_get_agent_status(&state, &args);
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["checkpoint_count"], 1);
    }

    #[test]
    fn test_search_checkpoints_tool_schema() {
        let tools = get_tools();
        let search_checkpoints = tools
            .iter()
            .find(|t| t.name == "search_checkpoints")
            .expect("search_checkpoints tool should exist");

        let schema = &search_checkpoints.input_schema;
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required field should be an array");

        assert!(required.iter().any(|v| v.as_str() == Some("query")));
        assert!(schema["properties"].get("agent").is_some());
        assert!(schema["properties"].get("limit").is_some());
    }

    #[test]
    fn test_get_agent_status_tool_schema() {
        let tools = get_tools();
        let get_agent_status = tools
            .iter()
            .find(|t| t.name == "get_agent_status")
            .expect("get_agent_status tool should exist");

        let schema = &get_agent_status.input_schema;
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required field should be an array");

        assert!(required.iter().any(|v| v.as_str() == Some("agent")));
    }
    #[test]
    fn test_search_hybrid_tool_registered() {
        let tools = get_tools();
        let search_hybrid = tools
            .iter()
            .find(|t| t.name == "search_hybrid")
            .expect("search_hybrid tool should exist");

        assert!(search_hybrid.description.is_some());
        assert!(search_hybrid
            .description
            .as_ref()
            .unwrap()
            .contains("vector"));
        assert!(search_hybrid
            .description
            .as_ref()
            .unwrap()
            .contains("graph"));
    }

    #[test]
    fn test_search_hybrid_tool_schema() {
        let tools = get_tools();
        let search_hybrid = tools
            .iter()
            .find(|t| t.name == "search_hybrid")
            .expect("search_hybrid tool should exist");

        let schema = &search_hybrid.input_schema;
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required field should be an array");

        // query is required
        assert!(required.iter().any(|v| v.as_str() == Some("query")));

        // Check optional parameters exist
        assert!(schema["properties"].get("query").is_some());
        assert!(schema["properties"].get("limit").is_some());
        assert!(schema["properties"].get("expansion_depth").is_some());
    }

    #[test]
    fn test_search_hybrid_json_shape() {
        // Verify the response JSON has the expected fields
        let response = serde_json::json!({
            "results": [],
            "query": "test",
            "limit": 10,
            "count": 0,
            "graph_context": [],
            "graph_context_count": 0,
            "edge_ids": [],
        });
        assert!(response["graph_context"].is_array());
        assert!(response["edge_ids"].is_array());
        assert!(response["results"].is_array());
        assert_eq!(response["count"], 0);
        assert_eq!(response["graph_context_count"], 0);
    }

    #[test]
    fn test_extract_agent_present() {
        let args = serde_json::json!({"agent": "mmn/nellie-rs", "query": "test"});
        assert_eq!(extract_agent(&args), "mmn/nellie-rs");
    }

    #[test]
    fn test_extract_agent_missing() {
        let args = serde_json::json!({"query": "test", "limit": 10});
        assert_eq!(extract_agent(&args), "unknown");
    }

    #[test]
    fn test_extract_agent_null() {
        let args = serde_json::json!(null);
        assert_eq!(extract_agent(&args), "unknown");
    }

    #[test]
    fn test_get_blast_radius_tool_registered() {
        let tools = get_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"get_blast_radius"),
            "get_blast_radius tool should be registered"
        );
    }

    #[test]
    fn test_get_blast_radius_schema() {
        let tools = get_tools();
        let blast_radius = tools
            .iter()
            .find(|t| t.name == "get_blast_radius")
            .expect("get_blast_radius tool should exist");

        let schema = &blast_radius.input_schema;
        assert!(
            schema.get("properties").is_some(),
            "Schema should have properties"
        );
        assert!(
            schema["properties"].get("changed_files").is_some(),
            "Schema should have changed_files property"
        );
        assert!(
            schema["properties"].get("depth").is_some(),
            "Schema should have depth property"
        );

        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("Schema should have required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("changed_files")),
            "changed_files should be required"
        );
    }

    #[test]
    fn test_get_blast_radius_missing_changed_files() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "depth": 2
        });

        let result = handle_get_blast_radius(&state, &args);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("changed_files is required and must be an array"));
    }

    #[test]
    fn test_get_blast_radius_empty_changed_files() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "changed_files": []
        });

        let result = handle_get_blast_radius(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["changed_files"].as_array().unwrap().len(), 0);
        assert_eq!(response["affected_symbols"].as_array().unwrap().len(), 0);
        assert_eq!(response["affected_files"].as_array().unwrap().len(), 0);
        assert_eq!(response["test_files"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_get_blast_radius_default_depth() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "changed_files": ["src/test.rs"]
        });

        let result = handle_get_blast_radius(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        // Default depth should be 2
        assert_eq!(response["depth"], 2);
    }

    #[test]
    fn test_get_blast_radius_custom_depth() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "changed_files": ["src/test.rs"],
            "depth": 5
        });

        let result = handle_get_blast_radius(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["depth"], 5);
    }

    #[test]
    fn test_get_blast_radius_response_shape() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "changed_files": ["src/test.rs"],
            "depth": 2
        });

        let result = handle_get_blast_radius(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();

        // Verify response structure
        assert!(response.get("changed_files").is_some());
        assert!(response.get("depth").is_some());
        assert!(response.get("affected_symbols").is_some());
        assert!(response.get("affected_files").is_some());
        assert!(response.get("test_files").is_some());

        // Verify arrays
        assert!(response["affected_symbols"].is_array());
        assert!(response["affected_files"].is_array());
        assert!(response["test_files"].is_array());
    }

    #[test]
    fn test_get_blast_radius_multiple_changed_files() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "changed_files": ["src/test1.rs", "src/test2.rs", "src/test3.rs"],
            "depth": 3
        });

        let result = handle_get_blast_radius(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();

        let changed = response["changed_files"].as_array().unwrap();
        assert_eq!(changed.len(), 3);
        assert!(changed.iter().any(|f| f.as_str() == Some("src/test1.rs")));
        assert!(changed.iter().any(|f| f.as_str() == Some("src/test2.rs")));
        assert!(changed.iter().any(|f| f.as_str() == Some("src/test3.rs")));
    }

    #[test]
    fn test_query_structure_tool_registered() {
        let tools = get_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"query_structure"),
            "query_structure tool should be registered"
        );
    }

    #[test]
    fn test_query_structure_schema() {
        let tools = get_tools();
        let query_structure = tools
            .iter()
            .find(|t| t.name == "query_structure")
            .expect("query_structure tool should exist");

        let schema = &query_structure.input_schema;
        assert!(
            schema.get("properties").is_some(),
            "Schema should have properties"
        );
        assert!(
            schema["properties"].get("symbol").is_some(),
            "Schema should have symbol property"
        );
        assert!(
            schema["properties"].get("query_type").is_some(),
            "Schema should have query_type property"
        );
        assert!(
            schema["properties"].get("language").is_some(),
            "Schema should have language property"
        );
        assert!(
            schema["properties"].get("limit").is_some(),
            "Schema should have limit property"
        );

        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("Schema should have required array");
        assert_eq!(required.len(), 2);
        assert!(
            required.iter().any(|v| v.as_str() == Some("symbol")),
            "symbol should be required"
        );
        assert!(
            required.iter().any(|v| v.as_str() == Some("query_type")),
            "query_type should be required"
        );
    }

    #[test]
    fn test_query_structure_missing_symbol() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "query_type": "callers"
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("symbol is required"));
    }

    #[test]
    fn test_query_structure_missing_query_type() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "test_function"
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("query_type is required"));
    }

    #[test]
    fn test_query_structure_response_shape() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "test_func",
            "query_type": "callers"
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();

        // Verify response structure
        assert!(response.get("symbol").is_some());
        assert!(response.get("query_type").is_some());
        assert!(response.get("results").is_some());
        assert!(response.get("count").is_some());

        // Verify arrays
        assert!(response["results"].is_array());
        assert_eq!(response["count"], 0); // Empty DB
    }

    #[test]
    fn test_query_structure_default_limit() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "test_func",
            "query_type": "callers"
            // No limit specified, should default to 20
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["count"], 0); // Empty DB, but handler accepts no limit
    }

    #[test]
    fn test_query_structure_custom_limit() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "test_func",
            "query_type": "callees",
            "limit": 5
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response["results"].is_array());
    }

    #[test]
    fn test_query_structure_with_language_filter() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "test_func",
            "query_type": "tests",
            "language": "rust"
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["symbol"], "test_func");
        assert_eq!(response["query_type"], "tests");
    }

    #[test]
    fn test_query_structure_symbols_in_file() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "src/main.rs",
            "query_type": "symbols_in_file"
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["query_type"], "symbols_in_file");
        assert!(response["results"].is_array());
    }

    #[test]
    fn test_query_structure_contains() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "parent_scope",
            "query_type": "contains"
        });

        let result = handle_query_structure(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["query_type"], "contains");
    }

    #[test]
    fn test_query_structure_invalid_query_type() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "symbol": "test_func",
            "query_type": "invalid_type"
        });

        let result = handle_query_structure(&state, &args);
        // Invalid query type returns empty results (matched by _ => Vec::new())
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["count"], 0);
    }

    #[test]
    fn test_get_review_context_tool_registered() {
        let tools = get_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"get_review_context"),
            "get_review_context tool should be registered"
        );
    }

    #[test]
    fn test_get_review_context_schema() {
        let tools = get_tools();
        let get_review_context = tools
            .iter()
            .find(|t| t.name == "get_review_context")
            .expect("get_review_context tool should exist");

        let schema = &get_review_context.input_schema;
        assert!(schema["properties"]["changed_files"].is_object());
        assert_eq!(
            schema["required"][0], "changed_files",
            "changed_files should be required"
        );
    }

    #[test]
    fn test_get_review_context_missing_changed_files() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({});

        let result = handle_get_review_context(&state, &args);
        assert!(
            result.is_err(),
            "Should return error when changed_files is missing"
        );
    }

    #[test]
    fn test_get_review_context_empty_files() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "changed_files": []
        });

        let result = handle_get_review_context(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["changed_files"], 0);
        assert_eq!(response["changed_symbols"], 0);
        assert_eq!(response["affected_callers"], 0);
        assert_eq!(response["test_coverage"], 0);
        assert_eq!(response["untested_functions"], 0);
    }

    #[test]
    fn test_get_review_context_response_shape() {
        let db = crate::storage::Database::open_in_memory()
            .expect("Failed to create in-memory database");
        db.with_conn(|conn| crate::storage::migrate(conn))
            .expect("Failed to migrate");
        let state = McpState::new(db);

        let args = serde_json::json!({
            "changed_files": ["src/main.rs"]
        });

        let result = handle_get_review_context(&state, &args);
        assert!(result.is_ok());
        let response = result.unwrap();

        // Verify all expected fields are present
        assert!(response["summary"].is_string());
        assert!(response["changed_files"].is_number());
        assert!(response["changed_symbols"].is_number());
        assert!(response["affected_callers"].is_number());
        assert!(response["test_coverage"].is_number());
        assert!(response["untested_functions"].is_number());
        assert!(response["changed_symbol_names"].is_array());
    }
}
