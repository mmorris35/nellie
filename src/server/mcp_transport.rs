//! MCP transport implementation using rmcp `StreamableHttpService`.
//!
//! This module provides the proper MCP protocol transport layer
//! that allows Claude Code, mcporter, and other MCP clients to connect.

use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::Value;

use crate::embeddings::EmbeddingService;
use crate::storage::Database;

// ==================== Request Types ====================

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchCodeRequest {
    #[schemars(description = "Natural language query to search for relevant code")]
    pub query: String,
    #[schemars(description = "Maximum number of results (default: 10)")]
    pub limit: Option<i32>,
    #[schemars(description = "Filter by programming language")]
    pub language: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchLessonsRequest {
    #[schemars(description = "Natural language query to search lessons")]
    pub query: String,
    #[schemars(description = "Maximum lessons to return (default: 5)")]
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListLessonsRequest {
    #[schemars(description = "Filter by severity level (critical, warning, info)")]
    pub severity: Option<String>,
    #[schemars(description = "Filter by repository name")]
    pub repo: Option<String>,
    #[schemars(description = "Maximum lessons to return (default: 50)")]
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddLessonRequest {
    #[schemars(description = "Brief title for the lesson")]
    pub title: String,
    #[schemars(description = "Full description of the lesson learned")]
    pub content: String,
    #[schemars(description = "Tags for categorization")]
    pub tags: Vec<String>,
    #[schemars(description = "Importance level (critical, warning, info)")]
    pub severity: Option<String>,
    #[schemars(description = "Repository name (e.g., mike-github/whag)")]
    pub repo: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteLessonRequest {
    #[schemars(description = "Lesson ID to delete")]
    pub id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddCheckpointRequest {
    #[schemars(description = "Agent identifier")]
    pub agent: String,
    #[schemars(description = "Current task description")]
    pub working_on: String,
    #[schemars(description = "State object to persist")]
    pub state: Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetCheckpointsRequest {
    #[schemars(
        description = "Agent identifier (optional — omit to get recent checkpoints across all agents)"
    )]
    pub agent: Option<String>,
    #[schemars(description = "Maximum checkpoints to return (default: 5)")]
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchCheckpointsRequest {
    #[schemars(description = "Query text to search checkpoints")]
    pub query: String,
    #[schemars(description = "Optional agent filter")]
    pub agent: Option<String>,
    #[schemars(description = "Maximum checkpoints to return (default: 5)")]
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetAgentStatusRequest {
    #[schemars(description = "Agent identifier")]
    pub agent: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TriggerReindexRequest {
    #[schemars(description = "File or directory path to re-index (optional)")]
    pub path: Option<String>,
}

// ==================== MCP Handler ====================

/// MCP server handler for Nellie.
#[derive(Clone)]
pub struct NellieMcpHandler {
    db: Database,
    embeddings: Option<EmbeddingService>,
    tool_router: ToolRouter<Self>,
}

impl NellieMcpHandler {
    /// Create a new MCP handler.
    pub fn new(db: Database, embeddings: Option<EmbeddingService>) -> Self {
        Self {
            db,
            embeddings,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl NellieMcpHandler {
    #[tool(description = "Search indexed code repositories for relevant code snippets")]
    fn search_code(&self, Parameters(req): Parameters<SearchCodeRequest>) -> String {
        let limit = req.limit.unwrap_or(10) as usize;

        let Some(ref embeddings) = self.embeddings else {
            return serde_json::json!({"error": "Embedding service not initialized"}).to_string();
        };

        if !embeddings.is_initialized() {
            return serde_json::json!({"error": "Embedding service not fully initialized"})
                .to_string();
        }

        // Generate embedding using a dedicated runtime to avoid blocking tokio
        let query_text = req.query.clone();
        let embeddings_clone = embeddings.clone();
        let embedding = match std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async { embeddings_clone.embed_one(query_text).await })
        })
        .join()
        {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => {
                return serde_json::json!({"error": format!("Embedding failed: {}", e)}).to_string()
            }
            Err(_) => return serde_json::json!({"error": "Embedding thread panicked"}).to_string(),
        };

        let mut search_opts = crate::storage::SearchOptions::new(limit);
        if let Some(lang) = req.language.as_ref() {
            search_opts = search_opts.with_language(lang);
        }

        match self
            .db
            .with_conn(|conn| crate::storage::search_chunks(conn, &embedding, &search_opts))
        {
            Ok(results) => {
                let formatted: Vec<Value> = results
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "file_path": r.record.file_path,
                            "chunk_index": r.record.chunk_index,
                            "start_line": r.record.start_line,
                            "end_line": r.record.end_line,
                            "content": r.record.content,
                            "language": r.record.language,
                            "score": r.score,
                        })
                    })
                    .collect();

                serde_json::json!({
                    "results": formatted,
                    "query": req.query,
                    "count": formatted.len(),
                })
                .to_string()
            }
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "Search previously recorded lessons learned")]
    fn search_lessons(&self, Parameters(req): Parameters<SearchLessonsRequest>) -> String {
        let limit = req.limit.unwrap_or(5) as usize;

        let Some(ref embeddings) = self.embeddings else {
            return serde_json::json!({"error": "Embedding service not initialized"}).to_string();
        };

        if !embeddings.is_initialized() {
            return serde_json::json!({"error": "Embedding service not fully initialized"})
                .to_string();
        }

        let embeddings_clone = embeddings.clone();
        let embedding = match std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async { embeddings_clone.embed_one(req.query.clone()).await })
        })
        .join()
        {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => {
                return serde_json::json!({"error": format!("Embedding failed: {}", e)}).to_string()
            }
            Err(_) => return serde_json::json!({"error": "Embedding thread panicked"}).to_string(),
        };

        match self
            .db
            .with_conn(|conn| crate::storage::search_lessons_by_embedding(conn, &embedding, limit))
        {
            Ok(lessons) => serde_json::to_string(&lessons).unwrap_or_else(|_| "[]".to_string()),
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "List all recorded lessons learned with optional filters")]
    fn list_lessons(&self, Parameters(req): Parameters<ListLessonsRequest>) -> String {
        let limit = req.limit.unwrap_or(50) as usize;

        let lessons = if let Some(sev) = req.severity.as_ref() {
            self.db
                .with_conn(|conn| crate::storage::list_lessons_by_severity(conn, sev))
        } else {
            self.db.with_conn(crate::storage::list_lessons)
        };

        match lessons {
            Ok(list) => {
                // Filter by repo if specified
                let filtered: Vec<_> = if let Some(ref repo) = req.repo {
                    list.into_iter()
                        .filter(|l| l.repo.as_ref() == Some(repo))
                        .take(limit)
                        .collect()
                } else {
                    list.into_iter().take(limit).collect()
                };
                serde_json::json!({
                    "lessons": filtered,
                    "count": filtered.len(),
                })
                .to_string()
            }
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "Record a lesson learned during development")]
    fn add_lesson(&self, Parameters(req): Parameters<AddLessonRequest>) -> String {
        let severity = req.severity.as_deref().unwrap_or("info");
        let mut lesson =
            crate::storage::LessonRecord::new(&req.title, &req.content, req.tags.clone())
                .with_severity(severity);
        if let Some(ref repo) = req.repo {
            lesson = lesson.with_repo(repo);
        }
        let id = lesson.id.clone();

        if let Err(e) = self
            .db
            .with_conn(|conn| crate::storage::insert_lesson(conn, &lesson))
        {
            return serde_json::json!({"error": e.to_string()}).to_string();
        }

        // Generate and store embedding if available
        if let Some(ref embeddings) = self.embeddings {
            if embeddings.is_initialized() {
                let text = format!("{}\n{}", lesson.title, lesson.content);
                let embeddings_clone = embeddings.clone();
                let lesson_id = lesson.id;
                let db = self.db.clone();
                std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Runtime::new() {
                        if let Ok(embedding) =
                            rt.block_on(async { embeddings_clone.embed_one(text).await })
                        {
                            let _ = db.with_conn(|conn| {
                                crate::storage::store_lesson_embedding(conn, &lesson_id, &embedding)
                            });
                        }
                    }
                });
            }
        }

        serde_json::json!({
            "id": id,
            "message": "Lesson recorded successfully"
        })
        .to_string()
    }

    #[tool(description = "Delete a lesson by ID")]
    fn delete_lesson(&self, Parameters(req): Parameters<DeleteLessonRequest>) -> String {
        match self
            .db
            .with_conn(|conn| crate::storage::delete_lesson(conn, &req.id))
        {
            Ok(()) => serde_json::json!({
                "id": req.id,
                "message": "Lesson deleted successfully"
            })
            .to_string(),
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "Store an agent checkpoint for context recovery")]
    fn add_checkpoint(&self, Parameters(req): Parameters<AddCheckpointRequest>) -> String {
        let checkpoint =
            crate::storage::CheckpointRecord::new(&req.agent, &req.working_on, req.state);
        let id = checkpoint.id.clone();

        if let Err(e) = self
            .db
            .with_conn(|conn| crate::storage::insert_checkpoint(conn, &checkpoint))
        {
            return serde_json::json!({"error": e.to_string()}).to_string();
        }

        // Generate and store embedding if available
        if let Some(ref embeddings) = self.embeddings {
            if embeddings.is_initialized() {
                let text = checkpoint.working_on;
                let embeddings_clone = embeddings.clone();
                let checkpoint_id = checkpoint.id;
                let db = self.db.clone();
                std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Runtime::new() {
                        if let Ok(embedding) =
                            rt.block_on(async { embeddings_clone.embed_one(text).await })
                        {
                            let _ = db.with_conn(|conn| {
                                crate::storage::store_checkpoint_embedding(
                                    conn,
                                    &checkpoint_id,
                                    &embedding,
                                )
                            });
                        }
                    }
                });
            }
        }

        serde_json::json!({
            "id": id,
            "message": "Checkpoint saved successfully"
        })
        .to_string()
    }

    #[tool(
        description = "Retrieve recent checkpoints, optionally filtered by agent. Ordered by time (newest first)."
    )]
    fn get_recent_checkpoints(&self, Parameters(req): Parameters<GetCheckpointsRequest>) -> String {
        let limit = req.limit.unwrap_or(5) as usize;

        let result = if let Some(ref agent) = req.agent {
            self.db
                .with_conn(|conn| crate::storage::get_recent_checkpoints(conn, agent, limit))
        } else {
            self.db
                .with_conn(|conn| crate::storage::get_recent_checkpoints_all(conn, limit))
        };

        match result {
            Ok(checkpoints) => {
                serde_json::to_string(&checkpoints).unwrap_or_else(|_| "[]".to_string())
            }
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "Search checkpoints semantically by query text")]
    fn search_checkpoints(&self, Parameters(req): Parameters<SearchCheckpointsRequest>) -> String {
        let limit = req.limit.unwrap_or(5) as usize;

        let Some(ref embeddings) = self.embeddings else {
            return serde_json::json!({"error": "Embedding service not initialized"}).to_string();
        };

        if !embeddings.is_initialized() {
            return serde_json::json!({"error": "Embedding service not fully initialized"})
                .to_string();
        }

        let query_text = req.query.clone();
        let embeddings_clone = embeddings.clone();
        let embedding = match std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async { embeddings_clone.embed_one(query_text).await })
        })
        .join()
        {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => {
                return serde_json::json!({"error": format!("Embedding failed: {}", e)}).to_string()
            }
            Err(_) => return serde_json::json!({"error": "Embedding thread panicked"}).to_string(),
        };

        match self.db.with_conn(|conn| {
            crate::storage::search_checkpoints_by_embedding(conn, &embedding, limit)
        }) {
            Ok(results) => {
                let checkpoints: Vec<_> = if let Some(ref agent_filter) = req.agent {
                    results
                        .into_iter()
                        .filter(|cp| cp.record.agent == *agent_filter)
                        .map(|cp| cp.record)
                        .collect()
                } else {
                    results.into_iter().map(|cp| cp.record).collect()
                };

                serde_json::json!({
                    "checkpoints": checkpoints,
                    "count": checkpoints.len(),
                    "query": req.query,
                })
                .to_string()
            }
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "Get quick status for an agent (idle/in_progress, current task)")]
    fn get_agent_status(&self, Parameters(req): Parameters<GetAgentStatusRequest>) -> String {
        match self
            .db
            .with_conn(|conn| crate::storage::get_agent_status(conn, &req.agent))
        {
            Ok(status) => serde_json::json!({
                "agent": status.agent,
                "status": status.status.as_str(),
                "current_task": status.current_task,
                "last_updated": status.last_updated,
                "checkpoint_count": status.checkpoint_count,
            })
            .to_string(),
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        }
    }

    #[tool(description = "Trigger manual re-indexing of specified paths")]
    fn trigger_reindex(&self, Parameters(req): Parameters<TriggerReindexRequest>) -> String {
        if let Some(target_path) = req.path.as_ref() {
            match self.db.with_conn(|conn| {
                crate::storage::delete_chunks_by_file(conn, target_path)?;
                crate::storage::delete_file_state(conn, target_path)?;
                Ok::<_, crate::Error>(())
            }) {
                Ok(_) => serde_json::json!({
                    "status": "reindex_scheduled",
                    "path": target_path,
                })
                .to_string(),
                Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
            }
        } else {
            match self.db.with_conn(|conn| {
                let paths = crate::storage::list_file_paths(conn)?;
                for file_path in paths {
                    crate::storage::delete_file_state(conn, &file_path)?;
                }
                Ok::<_, crate::Error>(())
            }) {
                Ok(_) => serde_json::json!({
                    "status": "reindex_scheduled",
                    "path": "all",
                })
                .to_string(),
                Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
            }
        }
    }

    #[tool(description = "Get Nellie server status and statistics")]
    fn get_status(&self) -> String {
        let chunk_count = self.db.with_conn(crate::storage::count_chunks).unwrap_or(0);
        let lesson_count = self
            .db
            .with_conn(crate::storage::count_lessons)
            .unwrap_or(0);
        let file_count = self
            .db
            .with_conn(crate::storage::count_tracked_files)
            .unwrap_or(0);

        serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "stats": {
                "chunks": chunk_count,
                "lessons": lesson_count,
                "files": file_count,
            }
        })
        .to_string()
    }
}

impl ServerHandler for NellieMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Nellie is a semantic code memory system. Use search_code to find code, \
                 search_lessons/add_lesson for lessons learned, and checkpoint tools for \
                 agent state recovery."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>>
           + Send
           + '_ {
        std::future::ready(Ok(rmcp::model::ListToolsResult {
            meta: None,
            tools: self.tool_router.list_all(),
            next_cursor: None,
        }))
    }

    fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::CallToolResult, rmcp::ErrorData>>
           + Send
           + '_ {
        async move {
            let tool_name = request.name.to_string();
            let start = std::time::Instant::now();

            // Extract agent from arguments if present
            let agent = request
                .arguments
                .as_ref()
                .and_then(|args| args.get("agent"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let span = tracing::info_span!(
                "mcp_tool_call",
                tool = %tool_name,
                agent = %agent,
                dispatch = "rmcp",
            );
            let _guard = span.enter();

            tracing::debug!("Invoking tool via rmcp: {}", tool_name);

            let result = self
                .tool_router
                .call(rmcp::handler::server::tool::ToolCallContext::new(
                    self, request, context,
                ))
                .await;

            let latency = start.elapsed();

            match &result {
                Ok(call_result) => {
                    // Estimate response size from content text
                    let response_bytes: usize = call_result
                        .content
                        .iter()
                        .map(|c| {
                            // Content items are typically RawContent::Text
                            // Serialize to get a size estimate
                            serde_json::to_string(c).map(|s| s.len()).unwrap_or(0)
                        })
                        .sum();

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
                        "rmcp tool call succeeded"
                    );
                }
                Err(e) => {
                    crate::server::metrics::record_tool_call(
                        &tool_name, &agent, "error", latency, 0,
                    );
                    tracing::warn!(
                        error = ?e,
                        latency_ms = latency.as_millis() as u64,
                        "rmcp tool call failed"
                    );
                }
            }

            result
        }
    }
}

/// MCP server configuration.
#[derive(Debug, Clone)]
pub struct McpTransportConfig {
    /// Host to bind to
    pub host: String,
    /// Port for MCP server
    pub port: u16,
}

impl Default for McpTransportConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8766,
        }
    }
}

/// Start the MCP HTTP server using StreamableHttpService.
///
/// This starts a server that speaks the MCP protocol,
/// allowing Claude Code, mcporter, and other MCP clients to connect.
pub async fn start_mcp_server(
    config: McpTransportConfig,
    db: Database,
    embeddings: Option<EmbeddingService>,
) -> crate::Result<tokio::task::JoinHandle<()>> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    use std::net::SocketAddr;
    use tokio_util::sync::CancellationToken;

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .map_err(|e| crate::Error::config(format!("Invalid MCP address: {e}")))?;

    tracing::info!(%addr, "Starting MCP HTTP server");

    let ct = CancellationToken::new();

    // Create the StreamableHttpService with a factory function
    let db_clone = db.clone();
    let embeddings_clone = embeddings.clone();

    let mcp_config = StreamableHttpServerConfig {
        stateful_mode: true,
        cancellation_token: ct.child_token(),
        ..Default::default()
    };

    let service: StreamableHttpService<NellieMcpHandler, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(NellieMcpHandler::new(
                    db_clone.clone(),
                    embeddings_clone.clone(),
                ))
            },
            Arc::new(LocalSessionManager::default()),
            mcp_config,
        );

    // Create router with MCP service
    let router = axum::Router::new().nest_service("/mcp", service);

    // Create TCP listener
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| crate::Error::config(format!("Failed to bind MCP server: {e}")))?;

    let ct_for_shutdown = ct.clone();
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct_for_shutdown.cancelled().await })
            .await
        {
            tracing::error!(error = %e, "MCP server error");
        }
    });

    tracing::info!(%addr, "MCP HTTP server started");
    tracing::info!("MCP endpoint: POST http://{}/mcp", addr);

    Ok(handle)
}
