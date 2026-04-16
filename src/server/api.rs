//! REST API v1 endpoints for the web dashboard.
//!
//! Provides JSON endpoints consumed by the htmx-powered web UI
//! and available for any REST client.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{sse::Event, IntoResponse, Sse},
    routing::{delete, get},
    Json, Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use super::mcp::McpState;
use crate::storage;

/// Dashboard statistics response.
#[derive(Debug, Serialize, Deserialize)]
pub struct DashboardStats {
    pub version: String,
    pub uptime_seconds: u64,
    pub chunks: i64,
    pub lessons: i64,
    pub tracked_files: i64,
    pub db_size_bytes: u64,
    pub embeddings_enabled: bool,
}

/// Paginated file list response.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileListResponse {
    pub files: Vec<FileEntry>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// Single file entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub chunks: i64,
}

/// Search request query parameters.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    20
}

/// Search result item.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub file_path: String,
    pub content: String,
    pub score: f32,
    pub chunk_index: i64,
}

/// Search response.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub query: String,
    pub total: usize,
}

/// Lesson list query parameters.
#[derive(Debug, Deserialize)]
pub struct LessonListQuery {
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

/// Lesson list response.
#[derive(Debug, Serialize, Deserialize)]
pub struct LessonListResponse {
    pub lessons: Vec<LessonEntry>,
    pub total: usize,
}

/// Single lesson entry for the UI.
#[derive(Debug, Serialize, Deserialize)]
pub struct LessonEntry {
    pub id: String,
    pub title: String,
    pub content: String,
    pub severity: String,
    pub tags: Vec<String>,
    pub created_at: i64,
}

/// Create lesson request body.
#[derive(Debug, Deserialize)]
pub struct CreateLessonRequest {
    pub title: String,
    pub content: String,
    #[serde(default = "default_severity")]
    pub severity: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_severity() -> String {
    "info".to_string()
}

/// Tool metrics summary for the dashboard.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolMetricsSummary {
    /// Total tool invocations across all tools and agents.
    pub total_invocations: u64,
    /// Total error invocations across all tools and agents.
    pub total_errors: u64,
    /// Estimated LLM tokens saved by all tool responses.
    pub estimated_tokens_saved: f64,
    /// Total response payload bytes across all tools.
    pub total_response_bytes: u64,
    /// Per-tool metrics breakdown.
    pub tools: Vec<ToolMetricsEntry>,
    /// Per-agent metrics breakdown.
    pub agents: Vec<AgentMetricsEntry>,
}

/// Per-tool metrics breakdown.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolMetricsEntry {
    /// Tool name (e.g., `"search_code"`).
    pub name: String,
    /// Total invocations for this tool.
    pub invocations: u64,
    /// Total error invocations for this tool.
    pub errors: u64,
    /// Average latency in milliseconds.
    pub avg_latency_ms: f64,
    /// 95th percentile latency in milliseconds.
    pub p95_latency_ms: f64,
    /// Estimated tokens saved by this tool's responses.
    pub tokens_saved: f64,
    /// Total response payload bytes for this tool.
    pub response_bytes: u64,
}

/// Per-agent metrics breakdown.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentMetricsEntry {
    /// Agent identifier (e.g., `"user/example"`).
    pub agent: String,
    /// Total invocations by this agent.
    pub invocations: u64,
    /// Estimated tokens saved for this agent.
    pub tokens_saved: f64,
}

/// Hybrid search query parameters.
#[derive(Debug, Deserialize)]
pub struct HybridSearchQuery {
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default = "default_expansion_depth")]
    pub expansion_depth: i64,
}

const fn default_expansion_depth() -> i64 {
    2
}

/// Hybrid search response combining vector results with graph context.
#[derive(Debug, Serialize, Deserialize)]
pub struct HybridSearchResponse {
    pub results: Vec<SearchResult>,
    pub graph_context: Vec<GraphContextEntry>,
    pub query: String,
    pub total: usize,
    pub graph_context_count: usize,
}

/// A single graph entity returned as context from hybrid search.
#[derive(Debug, Serialize, Deserialize)]
pub struct GraphContextEntry {
    pub label: String,
    pub entity_type: String,
    pub related_to: Vec<String>,
}

/// Checkpoint list query parameters.
#[derive(Debug, Deserialize)]
pub struct CheckpointQuery {
    /// Filter by agent name.
    pub agent: Option<String>,
    /// Search by text in `working_on`.
    pub q: Option<String>,
    /// Maximum number of results.
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Offset for pagination.
    #[serde(default)]
    pub offset: i64,
}

/// Single checkpoint entry for the UI.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointEntry {
    /// Unique checkpoint identifier.
    pub id: String,
    /// Agent that created this checkpoint.
    pub agent: String,
    /// Description of what the agent was working on.
    pub working_on: String,
    /// Agent state as JSON.
    pub state: serde_json::Value,
    /// Unix timestamp when created.
    pub created_at: i64,
}

/// Paginated checkpoint list response.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointListResponse {
    /// Checkpoint entries in the current page.
    pub checkpoints: Vec<CheckpointEntry>,
    /// Total number of matching checkpoints.
    pub total: usize,
}

/// Agent list response.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentListResponse {
    /// List of agents with checkpoint activity.
    pub agents: Vec<AgentEntry>,
}

/// Single agent entry with checkpoint counts.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Agent name/identifier.
    pub name: String,
    /// Total number of checkpoints for this agent.
    pub checkpoint_count: i64,
    /// Unix timestamp of the most recent checkpoint.
    pub last_active: i64,
}

/// Pagination query parameters.
#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

/// Graph query parameters for the knowledge graph explorer.
#[derive(Debug, Deserialize)]
pub struct GraphQueryParams {
    pub label: Option<String>,
    #[serde(default = "default_graph_limit")]
    pub limit: i64,
}

fn default_graph_limit() -> i64 {
    50
}

/// Knowledge graph response for vis.js visualization.
#[derive(Debug, Serialize, Deserialize)]
pub struct GraphResponse {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub total_nodes: usize,
    pub total_edges: usize,
}

/// A node in the knowledge graph visualization.
#[derive(Debug, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub entity_type: String,
    pub weight: f64,
}

/// An edge in the knowledge graph visualization.
#[derive(Debug, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub relationship: String,
    pub weight: f64,
}

/// Application start time for uptime calculation.
static START_TIME: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Initialize the start time. Call once at server startup.
pub fn init_start_time() {
    START_TIME.get_or_init(std::time::Instant::now);
}

/// Create the dashboard API router.
pub fn create_api_router(state: Arc<McpState>) -> Router {
    // Initialize start time on first router creation
    init_start_time();

    Router::new()
        .route("/api/v1/dashboard", get(dashboard_stats))
        .route("/api/v1/files", get(list_files))
        .route("/api/v1/search", get(search))
        .route("/api/v1/lessons", get(list_lessons).post(create_lesson))
        .route("/api/v1/lessons/{id}", delete(delete_lesson))
        .route("/api/v1/metrics", get(tool_metrics))
        .route("/api/v1/search/hybrid", get(hybrid_search))
        .route("/api/v1/checkpoints", get(list_checkpoints))
        .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/graph", get(graph_query))
        .route("/api/v1/activity", get(activity_stream))
        .with_state(state)
}

/// GET /api/v1/dashboard - Dashboard statistics.
async fn dashboard_stats(State(state): State<Arc<McpState>>) -> impl IntoResponse {
    let chunks = state.db().with_conn(storage::count_chunks).unwrap_or(0);
    let lessons = state.db().with_conn(storage::count_lessons).unwrap_or(0);
    let tracked_files = state
        .db()
        .with_conn(storage::count_tracked_files)
        .unwrap_or(0);

    let db_size_bytes = std::fs::metadata(state.db().path()).map_or(0, |m| m.len());

    let uptime_seconds = START_TIME.get().map_or(0, |t| t.elapsed().as_secs());

    let embeddings_enabled = state.embeddings.is_some();

    tracing::debug!(
        chunks,
        lessons,
        tracked_files,
        db_size_bytes,
        "Dashboard stats requested"
    );

    Json(DashboardStats {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds,
        chunks,
        lessons,
        tracked_files,
        db_size_bytes,
        embeddings_enabled,
    })
}

/// GET /api/v1/files - List indexed files with pagination.
async fn list_files(
    State(state): State<Arc<McpState>>,
    Query(params): Query<PaginationQuery>,
) -> impl IntoResponse {
    let total = state
        .db()
        .with_conn(storage::count_tracked_files)
        .unwrap_or(0);

    let files = state
        .db()
        .with_conn(storage::list_file_paths)
        .unwrap_or_default();

    // Apply pagination manually (storage returns all paths)
    let offset = params.offset.max(0) as usize;
    let limit = params.limit.clamp(1, 200) as usize;

    let page: Vec<FileEntry> = files
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|path| {
            let chunks = state
                .db()
                .with_conn(|conn| storage::count_chunks_for_file(conn, &path))
                .unwrap_or(0);
            FileEntry { path, chunks }
        })
        .collect();

    Json(FileListResponse {
        files: page,
        total,
        offset: params.offset,
        limit: params.limit,
    })
}

/// GET /api/v1/search - Search code chunks.
async fn search(
    State(state): State<Arc<McpState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, StatusCode> {
    if params.q.trim().is_empty() {
        return Ok(Json(SearchResponse {
            results: Vec::new(),
            query: params.q,
            total: 0,
        }));
    }

    let limit = params.limit.clamp(1, 100);

    // Try semantic search if embeddings available
    if let Some(ref embedding_service) = state.embeddings {
        match embedding_service.embed_one(params.q.clone()).await {
            Ok(query_embedding) => {
                let results = state
                    .db()
                    .with_conn(|conn| {
                        let options = storage::SearchOptions {
                            limit: limit as usize,
                            ..Default::default()
                        };
                        storage::search_chunks(conn, &query_embedding, &options)
                    })
                    .map_err(|e| {
                        tracing::error!(error = %e, "Search failed");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;

                let search_results: Vec<SearchResult> = results
                    .into_iter()
                    .map(|r| SearchResult {
                        file_path: r.record.file_path,
                        content: r.record.content,
                        score: r.score,
                        chunk_index: r.record.chunk_index as i64,
                    })
                    .collect();

                let total = search_results.len();
                return Ok(Json(SearchResponse {
                    results: search_results,
                    query: params.q,
                    total,
                }));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Embedding failed, falling back to text search"
                );
            }
        }
    }

    // Fallback to text search
    let results = state
        .db()
        .with_conn(|conn| {
            let options = storage::SearchOptions {
                limit: limit as usize,
                ..Default::default()
            };
            storage::search_chunks_by_text(conn, &params.q, &options)
        })
        .map_err(|e| {
            tracing::error!(error = %e, "Text search failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let search_results: Vec<SearchResult> = results
        .into_iter()
        .map(|r| SearchResult {
            file_path: r.record.file_path,
            content: r.record.content,
            score: r.score,
            chunk_index: r.record.chunk_index as i64,
        })
        .collect();

    let total = search_results.len();
    Ok(Json(SearchResponse {
        results: search_results,
        query: params.q,
        total,
    }))
}

/// GET /api/v1/lessons - List lessons with optional filters.
async fn list_lessons(
    State(state): State<Arc<McpState>>,
    Query(params): Query<LessonListQuery>,
) -> impl IntoResponse {
    let lessons = if let Some(ref severity) = params.severity {
        state
            .db()
            .with_conn(|conn| storage::list_lessons_by_severity(conn, severity))
            .unwrap_or_default()
    } else {
        state
            .db()
            .with_conn(storage::list_lessons)
            .unwrap_or_default()
    };

    let total = lessons.len();

    let offset = params.offset.max(0) as usize;
    let limit = params.limit.clamp(1, 200) as usize;

    let page: Vec<LessonEntry> = lessons
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|l| LessonEntry {
            id: l.id,
            title: l.title,
            content: l.content,
            severity: l.severity,
            tags: l.tags,
            created_at: l.created_at,
        })
        .collect();

    Json(LessonListResponse {
        lessons: page,
        total,
    })
}

/// POST /api/v1/lessons - Create a new lesson.
async fn create_lesson(
    State(state): State<Arc<McpState>>,
    Json(body): Json<CreateLessonRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    let lesson = storage::LessonRecord {
        id: id.clone(),
        title: body.title,
        content: body.content.clone(),
        embedding: None,
        tags: body.tags,
        severity: body.severity,
        agent: None,
        repo: None,
        created_at: now,
        updated_at: now,
    };

    state
        .db()
        .with_conn(|conn| storage::insert_lesson(conn, &lesson))
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to create lesson");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Generate and store embedding if service available
    if let Some(ref embedding_service) = state.embeddings {
        let embed_text = format!("{} {}", lesson.title, body.content);
        match embedding_service.embed_one(embed_text).await {
            Ok(embedding) => {
                let _ = state
                    .db()
                    .with_conn(|conn| storage::store_lesson_embedding(conn, &id, &embedding));
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to generate lesson embedding");
            }
        }
    }

    tracing::info!(id = %id, "Lesson created via UI");

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "status": "created" })),
    ))
}

/// DELETE /api/v1/lessons/:id - Delete a lesson.
async fn delete_lesson(
    State(state): State<Arc<McpState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    state
        .db()
        .with_conn(|conn| storage::delete_lesson(conn, &id))
        .map_err(|e| {
            tracing::error!(error = %e, lesson_id = %id, "Failed to delete lesson");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(id = %id, "Lesson deleted via UI");
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/metrics - Tool-level metrics as structured JSON.
///
/// Returns a summary of all tool invocations, latencies, token savings,
/// and response bytes, broken down by tool name and agent.
async fn tool_metrics() -> impl IntoResponse {
    let summary = crate::server::metrics::collect_tool_metrics();
    Json(summary)
}

/// Convert storage search results to API search results.
fn to_search_results(results: &[storage::SearchResult<storage::ChunkRecord>]) -> Vec<SearchResult> {
    results
        .iter()
        .map(|r| SearchResult {
            file_path: r.record.file_path.clone(),
            content: r.record.content.clone(),
            score: r.score,
            chunk_index: i64::from(r.record.chunk_index),
        })
        .collect()
}

/// Run text-based chunk search as a fallback.
///
/// Returns empty results if text search is not available rather
/// than failing the entire request.
fn run_text_search(
    state: &McpState,
    query: &str,
    limit: usize,
) -> Vec<storage::SearchResult<storage::ChunkRecord>> {
    let options = storage::SearchOptions {
        limit,
        ..Default::default()
    };
    state
        .db()
        .with_conn(|conn| storage::search_chunks_by_text(conn, query, &options))
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "Hybrid search: text search unavailable, returning empty results"
            );
            Vec::new()
        })
}

/// Expand search results through the knowledge graph.
fn expand_graph_context(
    graph_lock: &std::sync::Arc<parking_lot::RwLock<crate::graph::GraphMemory>>,
    query: &str,
    vector_results: &[storage::SearchResult<storage::ChunkRecord>],
    expansion_depth: usize,
) -> Vec<GraphContextEntry> {
    // Hold the read lock only while querying the graph
    let all_results = {
        let graph = graph_lock.read();
        let mut start_node_ids: Vec<String> = graph.fuzzy_match(query);

        for result in vector_results {
            if let Some(record_id) = result.record.id {
                let chunk_nodes = graph.find_by_record_id(&record_id.to_string());
                start_node_ids.extend(chunk_nodes);
            }
        }
        start_node_ids.dedup();

        let mut results = Vec::new();
        for start_id in &start_node_ids {
            let qrs = crate::graph::GraphQuery::new(&graph)
                .label(start_id)
                .direction(crate::graph::Direction::Both)
                .depth(expansion_depth)
                .min_confidence(0.3)
                .limit(20)
                .execute();
            results.extend(qrs);
        }
        drop(graph);
        results
    };

    // Transform query results into API response entries
    let mut context = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for qr in all_results {
        if seen.insert(qr.entity.id.clone()) {
            let related_to: Vec<String> = qr.path.iter().map(|e| e.relationship.clone()).collect();
            context.push(GraphContextEntry {
                label: qr.entity.label,
                entity_type: qr.entity.entity_type,
                related_to,
            });
        }
    }
    context
}

/// GET /api/v1/search/hybrid - Hybrid search combining vector search
/// with graph expansion.
async fn hybrid_search(
    State(state): State<Arc<McpState>>,
    Query(params): Query<HybridSearchQuery>,
) -> Result<Json<HybridSearchResponse>, StatusCode> {
    if params.q.trim().is_empty() {
        return Ok(Json(HybridSearchResponse {
            results: Vec::new(),
            graph_context: Vec::new(),
            query: params.q,
            total: 0,
            graph_context_count: 0,
        }));
    }

    let limit = usize::try_from(params.limit.clamp(1, 100)).unwrap_or(100);
    let expansion_depth = usize::try_from(params.expansion_depth.clamp(0, 5)).unwrap_or(2);

    // Step 1: Vector search (semantic or text fallback)
    let vector_results = match state.embeddings {
        Some(ref svc) => match svc.embed_one(params.q.clone()).await {
            Ok(emb) => {
                let opts = storage::SearchOptions {
                    limit,
                    ..Default::default()
                };
                state
                    .db()
                    .with_conn(|conn| storage::search_chunks(conn, &emb, &opts))
                    .map_err(|e| {
                        tracing::error!(error = %e, "Hybrid search: vector search failed");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?
            }
            Err(e) => {
                tracing::warn!(error = %e, "Hybrid search: embedding failed, text fallback");
                run_text_search(&state, &params.q, limit)
            }
        },
        None => run_text_search(&state, &params.q, limit),
    };

    let search_results = to_search_results(&vector_results);

    // Step 2: Graph expansion (if graph is enabled)
    let graph_context = state.graph.as_ref().map_or_else(Vec::new, |graph_lock| {
        expand_graph_context(graph_lock, &params.q, &vector_results, expansion_depth)
    });

    let total = search_results.len();
    let graph_context_count = graph_context.len();

    Ok(Json(HybridSearchResponse {
        results: search_results,
        graph_context,
        query: params.q,
        total,
        graph_context_count,
    }))
}

/// Convert storage `CheckpointRecord` to API `CheckpointEntry`.
fn to_checkpoint_entry(cp: storage::CheckpointRecord) -> CheckpointEntry {
    CheckpointEntry {
        id: cp.id,
        agent: cp.agent,
        working_on: cp.working_on,
        state: cp.state,
        created_at: cp.created_at,
    }
}

/// GET /api/v1/checkpoints - List checkpoints with optional agent/text filters.
///
/// Supports three modes:
/// - No filters: returns recent checkpoints across all agents
/// - `agent` param: filters by agent name
/// - `q` param: searches by text in `working_on`
async fn list_checkpoints(
    State(state): State<Arc<McpState>>,
    Query(params): Query<CheckpointQuery>,
) -> impl IntoResponse {
    let limit = usize::try_from(params.limit.clamp(1, 200)).unwrap_or(20);

    let checkpoints = if let Some(ref agent) = params.agent {
        state
            .db()
            .with_conn(|conn| storage::search_checkpoints_by_agent(conn, agent, limit))
            .unwrap_or_default()
    } else if let Some(ref q) = params.q {
        state
            .db()
            .with_conn(|conn| storage::search_checkpoints_by_text(conn, q, limit))
            .unwrap_or_default()
    } else {
        state
            .db()
            .with_conn(|conn| storage::get_recent_checkpoints_all(conn, limit))
            .unwrap_or_default()
    };

    let total = checkpoints.len();
    let offset = usize::try_from(params.offset.max(0))
        .unwrap_or(0)
        .min(total);

    let page: Vec<CheckpointEntry> = checkpoints
        .into_iter()
        .skip(offset)
        .map(to_checkpoint_entry)
        .collect();

    Json(CheckpointListResponse {
        checkpoints: page,
        total,
    })
}

/// GET /api/v1/agents - List distinct agents with checkpoint counts.
///
/// Returns agents ordered by most recently active first.
async fn list_agents(State(state): State<Arc<McpState>>) -> impl IntoResponse {
    let agents = state
        .db()
        .with_conn(storage::list_distinct_agents)
        .unwrap_or_default();

    let entries: Vec<AgentEntry> = agents
        .into_iter()
        .map(|a| AgentEntry {
            name: a.name,
            checkpoint_count: a.checkpoint_count,
            last_active: a.last_active,
        })
        .collect();

    Json(AgentListResponse { agents: entries })
}

/// Build a full graph response (no label filter) from all entities and edges.
///
/// Returns up to `limit` nodes and all edges between those nodes.
fn build_full_graph(graph: &crate::graph::GraphMemory, limit: usize) -> GraphResponse {
    let all_entities = graph.all_entities();
    let total_nodes = all_entities.len();

    let nodes: Vec<GraphNode> = all_entities
        .into_iter()
        .take(limit)
        .map(|e| GraphNode {
            id: e.id.clone(),
            label: e.label.clone(),
            entity_type: e.entity_type.to_string(),
            weight: f64::from(e.access_count),
        })
        .collect();

    // Collect node IDs in the response for edge filtering
    let node_ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    let all_rels = graph.all_relationships();
    let total_edges = all_rels.len();

    let edges: Vec<GraphEdge> = all_rels
        .into_iter()
        .filter(|(from, to, _)| node_ids.contains(from) && node_ids.contains(to))
        .map(|(from, to, rel)| GraphEdge {
            from: from.to_string(),
            to: to.to_string(),
            relationship: rel.kind.to_string(),
            weight: f64::from(rel.confidence),
        })
        .collect();

    GraphResponse {
        nodes,
        edges,
        total_nodes,
        total_edges,
    }
}

/// Build a neighborhood graph response for a specific label.
///
/// Finds entities matching the label and traverses outward to collect
/// nearby nodes and the edges connecting them.
fn build_label_graph(
    graph: &crate::graph::GraphMemory,
    label: &str,
    limit: usize,
) -> GraphResponse {
    let query_results = crate::graph::GraphQuery::new(graph)
        .label(label)
        .direction(crate::graph::Direction::Both)
        .depth(2)
        .min_confidence(0.0)
        .limit(limit)
        .execute();

    // Collect the start nodes (fuzzy-matched by label)
    let start_ids = graph.fuzzy_match(label);
    let mut seen_ids: std::collections::HashSet<String> = start_ids.iter().cloned().collect();

    let mut nodes: Vec<GraphNode> = Vec::new();

    // Add start nodes
    for start_id in &start_ids {
        if let Some(entity) = graph.get_entity(start_id) {
            nodes.push(GraphNode {
                id: entity.id.clone(),
                label: entity.label.clone(),
                entity_type: entity.entity_type.to_string(),
                weight: f64::from(entity.access_count),
            });
        }
    }

    // Add discovered neighbor nodes
    for qr in &query_results {
        if seen_ids.insert(qr.entity.id.clone()) {
            nodes.push(GraphNode {
                id: qr.entity.id.clone(),
                label: qr.entity.label.clone(),
                entity_type: qr.entity.entity_type.clone(),
                weight: qr.depth as f64,
            });
        }
    }

    let total_nodes = nodes.len();

    // Collect edges between all discovered nodes
    let node_id_set: std::collections::HashSet<&str> =
        nodes.iter().map(|n| n.id.as_str()).collect();

    let all_rels = graph.all_relationships();
    let total_edges = all_rels.len();

    let edges: Vec<GraphEdge> = all_rels
        .into_iter()
        .filter(|(from, to, _)| node_id_set.contains(from) && node_id_set.contains(to))
        .map(|(from, to, rel)| GraphEdge {
            from: from.to_string(),
            to: to.to_string(),
            relationship: rel.kind.to_string(),
            weight: f64::from(rel.confidence),
        })
        .collect();

    GraphResponse {
        nodes,
        edges,
        total_nodes,
        total_edges,
    }
}

/// GET /api/v1/graph - Query the knowledge graph for visualization.
///
/// If `label` param is present, returns the neighborhood of matching entities.
/// Otherwise returns the full graph (up to `limit` nodes).
/// Returns an empty response when the graph is disabled.
async fn graph_query(
    State(state): State<Arc<McpState>>,
    Query(params): Query<GraphQueryParams>,
) -> impl IntoResponse {
    let limit = usize::try_from(params.limit.clamp(1, 500)).unwrap_or(50);

    let response = match state.graph.as_ref() {
        Some(graph_lock) => {
            let graph = graph_lock.read();
            if let Some(ref label) = params.label {
                build_label_graph(&graph, label, limit)
            } else {
                build_full_graph(&graph, limit)
            }
        }
        None => GraphResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            total_nodes: 0,
            total_edges: 0,
        },
    };

    Json(response)
}

/// GET /api/v1/activity - SSE stream of server activity.
///
/// Sends periodic status updates that the dashboard polls.
/// Events include: indexing progress, search queries, health,
/// and tool invocation metrics.
async fn activity_stream(
    State(state): State<Arc<McpState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        let mut tick_count: u64 = 0;
        loop {
            interval.tick().await;
            tick_count += 1;

            let chunks = state
                .db()
                .with_conn(storage::count_chunks)
                .unwrap_or(0);
            let files = state
                .db()
                .with_conn(storage::count_tracked_files)
                .unwrap_or(0);
            let lessons = state
                .db()
                .with_conn(storage::count_lessons)
                .unwrap_or(0);

            let now = chrono::Utc::now().timestamp();

            let data = serde_json::json!({
                "type": "stats",
                "chunks": chunks,
                "files": files,
                "lessons": lessons,
                "timestamp": now,
            });

            yield Ok(Event::default()
                .event("activity")
                .data(data.to_string()));

            // Emit tool_activity event every 5th tick (~10 seconds)
            if tick_count % 5 == 0 {
                let tool_data = build_tool_activity_event(now);
                yield Ok(Event::default()
                    .event("activity")
                    .data(tool_data.to_string()));
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Build a tool activity SSE event from current prometheus metrics.
fn build_tool_activity_event(timestamp: i64) -> serde_json::Value {
    let summary = crate::server::metrics::collect_tool_metrics();
    let most_active = summary
        .tools
        .first()
        .map(|t| t.name.clone())
        .unwrap_or_default();

    serde_json::json!({
        "type": "tool_activity",
        "total_invocations": summary.total_invocations,
        "most_active_tool": most_active,
        "recent_errors": summary.total_errors,
        "estimated_tokens_saved": summary.estimated_tokens_saved,
        "timestamp": timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{migrate, Database};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn create_test_state() -> Arc<McpState> {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| migrate(conn)).unwrap();
        Arc::new(McpState::new(db))
    }

    #[tokio::test]
    async fn test_dashboard_stats() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let stats: DashboardStats = serde_json::from_slice(&body).unwrap();
        assert_eq!(stats.chunks, 0);
        assert_eq!(stats.lessons, 0);
        assert_eq!(stats.tracked_files, 0);
        assert!(!stats.embeddings_enabled);
    }

    #[tokio::test]
    async fn test_list_files_empty() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/files?limit=10&offset=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let files: FileListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(files.total, 0);
        assert!(files.files.is_empty());
    }

    #[tokio::test]
    async fn test_search_empty_query() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/search?q=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let search: SearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(search.total, 0);
    }

    #[tokio::test]
    async fn test_list_lessons_empty() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/lessons?limit=10&offset=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let lessons: LessonListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(lessons.total, 0);
    }

    #[tokio::test]
    async fn test_create_and_delete_lesson() {
        let state = create_test_state();
        let app = create_api_router(Arc::clone(&state));

        // Create a lesson
        let create_body = serde_json::json!({
            "title": "Test Lesson",
            "content": "This is a test lesson.",
            "severity": "info",
            "tags": ["test", "example"]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/lessons")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&create_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lesson_id = result["id"].as_str().unwrap().to_string();

        // Delete the lesson
        let app2 = create_api_router(state);
        let response = app2
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/lessons/{lesson_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_dashboard_stats_has_version() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let stats: DashboardStats = serde_json::from_slice(&body).unwrap();
        assert!(!stats.version.is_empty());
        assert!(stats.uptime_seconds < 60); // Test runs in under a minute
    }

    #[tokio::test]
    async fn test_tool_metrics_endpoint() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let summary: ToolMetricsSummary = serde_json::from_slice(&body).unwrap();

        // Even with no tool calls, the summary should deserialize
        // and have valid default-like values
        assert!(summary.tools.is_empty() || summary.total_invocations > 0);
    }

    #[tokio::test]
    async fn test_hybrid_search_empty_query() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/search/hybrid?q=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: HybridSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 0);
        assert!(result.results.is_empty());
        assert!(result.graph_context.is_empty());
        assert_eq!(result.graph_context_count, 0);
    }

    #[tokio::test]
    async fn test_hybrid_search_no_embeddings() {
        let state = create_test_state();
        let app = create_api_router(state);

        // With no embeddings, falls back to text search (returns empty on fresh DB)
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/search/hybrid?q=test&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: HybridSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.query, "test");
        assert!(result.graph_context.is_empty());
        assert_eq!(result.graph_context_count, 0);
    }

    #[tokio::test]
    async fn test_hybrid_search_with_expansion_depth() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/search/hybrid?q=hello&limit=5&expansion_depth=3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: HybridSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.query, "hello");
    }

    #[tokio::test]
    async fn test_list_checkpoints_empty() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/checkpoints?limit=10&offset=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: CheckpointListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 0);
        assert!(result.checkpoints.is_empty());
    }

    #[tokio::test]
    async fn test_list_checkpoints_with_data() {
        let state = create_test_state();

        // Insert a checkpoint
        state
            .db()
            .with_conn(|conn| {
                let cp = storage::CheckpointRecord::new(
                    "test-agent",
                    "Working on tests",
                    serde_json::json!({"key": "value"}),
                );
                storage::insert_checkpoint(conn, &cp)
            })
            .unwrap();

        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/checkpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: CheckpointListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.checkpoints[0].agent, "test-agent");
        assert_eq!(result.checkpoints[0].working_on, "Working on tests");
    }

    #[tokio::test]
    async fn test_list_checkpoints_filter_by_agent() {
        let state = create_test_state();

        state
            .db()
            .with_conn(|conn| {
                let cp1 =
                    storage::CheckpointRecord::new("agent-a", "Task A", serde_json::json!({}));
                let cp2 =
                    storage::CheckpointRecord::new("agent-b", "Task B", serde_json::json!({}));
                storage::insert_checkpoint(conn, &cp1)?;
                storage::insert_checkpoint(conn, &cp2)
            })
            .unwrap();

        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/checkpoints?agent=agent-a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: CheckpointListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.checkpoints[0].agent, "agent-a");
    }

    #[tokio::test]
    async fn test_list_checkpoints_search_by_text() {
        let state = create_test_state();

        state
            .db()
            .with_conn(|conn| {
                let cp1 = storage::CheckpointRecord::new(
                    "agent-1",
                    "Implementing feature X",
                    serde_json::json!({}),
                );
                let cp2 = storage::CheckpointRecord::new(
                    "agent-2",
                    "Debugging tests",
                    serde_json::json!({}),
                );
                storage::insert_checkpoint(conn, &cp1)?;
                storage::insert_checkpoint(conn, &cp2)
            })
            .unwrap();

        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/checkpoints?q=feature")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: CheckpointListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 1);
        assert!(result.checkpoints[0].working_on.contains("feature"));
    }

    #[tokio::test]
    async fn test_list_agents_empty() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: AgentListResponse = serde_json::from_slice(&body).unwrap();
        assert!(result.agents.is_empty());
    }

    #[tokio::test]
    async fn test_list_agents_with_data() {
        let state = create_test_state();

        state
            .db()
            .with_conn(|conn| {
                let cp1 =
                    storage::CheckpointRecord::new("agent-alpha", "Task 1", serde_json::json!({}));
                let cp2 =
                    storage::CheckpointRecord::new("agent-alpha", "Task 2", serde_json::json!({}));
                let cp3 =
                    storage::CheckpointRecord::new("agent-beta", "Task 3", serde_json::json!({}));
                storage::insert_checkpoint(conn, &cp1)?;
                storage::insert_checkpoint(conn, &cp2)?;
                storage::insert_checkpoint(conn, &cp3)
            })
            .unwrap();

        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: AgentListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.agents.len(), 2);

        // Should be ordered by last_active DESC
        // Find the agent-alpha entry — it should have 2 checkpoints
        let alpha = result
            .agents
            .iter()
            .find(|a| a.name == "agent-alpha")
            .expect("agent-alpha should exist");
        assert_eq!(alpha.checkpoint_count, 2);
        assert!(alpha.last_active > 0);

        let beta = result
            .agents
            .iter()
            .find(|a| a.name == "agent-beta")
            .expect("agent-beta should exist");
        assert_eq!(beta.checkpoint_count, 1);
    }

    #[tokio::test]
    async fn test_activity_stream() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/activity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify SSE content-type header
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/event-stream"));
    }

    #[tokio::test]
    async fn test_graph_query_no_graph() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: GraphResponse = serde_json::from_slice(&body).unwrap();
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
        assert_eq!(result.total_nodes, 0);
        assert_eq!(result.total_edges, 0);
    }

    #[tokio::test]
    async fn test_graph_query_with_label() {
        let state = create_test_state();
        let app = create_api_router(state);

        // Even without a graph, the endpoint should return empty gracefully
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/graph?label=test_entity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: GraphResponse = serde_json::from_slice(&body).unwrap();
        assert!(result.nodes.is_empty());
        assert_eq!(result.total_nodes, 0);
    }

    #[tokio::test]
    async fn test_graph_query_with_limit() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/graph?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: GraphResponse = serde_json::from_slice(&body).unwrap();
        // No graph configured, so empty
        assert!(result.nodes.is_empty());
    }
}
