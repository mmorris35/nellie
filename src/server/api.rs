//! REST API v1 endpoints for the web dashboard.
//!
//! Provides JSON endpoints consumed by the htmx-powered web UI
//! and available for any REST client.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{sse::Event, IntoResponse, Response, Sse},
    routing::{get, post},
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

/// Lesson search query parameters.
#[derive(Debug, Deserialize)]
pub struct LessonSearchQuery {
    pub q: String,
    #[serde(default = "default_lesson_search_limit")]
    pub limit: i64,
}

fn default_lesson_search_limit() -> i64 {
    10
}

/// Lesson search response.
#[derive(Debug, Serialize, Deserialize)]
pub struct LessonSearchResponse {
    pub lessons: Vec<LessonSearchEntry>,
    pub query: String,
    pub total: usize,
    pub search_type: String,
}

/// Lesson search result entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct LessonSearchEntry {
    pub id: String,
    pub title: String,
    pub content: String,
    pub severity: String,
    pub tags: Vec<String>,
    pub created_at: i64,
    /// Fused relevance in `[0, 1]` (see `storage::LessonSearchHit::score`);
    /// absent for substring-match fallback results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// Cosine similarity between query and lesson vectors; `null` if the
    /// lesson was found by keyword search only.
    #[serde(default)]
    pub similarity: Option<f32>,
    /// 1-based position in the keyword (BM25) ranking; `null` if the lesson
    /// was found by vector search only.
    #[serde(default)]
    pub keyword_rank: Option<usize>,
}

/// Embedding backfill response.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackfillResponse {
    pub processed: usize,
    pub skipped: usize,
    pub failed: usize,
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

/// Delete lesson query parameters.
#[derive(Debug, Deserialize)]
pub struct DeleteLessonQuery {
    /// Id (or unique id prefix) of the lesson that replaces the deleted one.
    #[serde(default)]
    pub successor: Option<String>,
    /// Why the lesson was deleted.
    #[serde(default)]
    pub reason: Option<String>,
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

/// Create checkpoint request body.
#[derive(Debug, Deserialize)]
pub struct CreateCheckpointRequest {
    pub agent: String,
    pub working_on: String,
    #[serde(default = "default_checkpoint_state")]
    pub state: serde_json::Value,
}

fn default_checkpoint_state() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Checkpoint search query parameters.
#[derive(Debug, Deserialize)]
pub struct CheckpointSearchQuery {
    pub q: String,
    pub agent: Option<String>,
    #[serde(default = "default_checkpoint_search_limit")]
    pub limit: i64,
}

fn default_checkpoint_search_limit() -> i64 {
    5
}

/// Checkpoint search response.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointSearchResponse {
    pub checkpoints: Vec<CheckpointSearchEntry>,
    pub query: String,
    pub total: usize,
    pub search_type: String,
}

/// Checkpoint search result entry (includes score for semantic results).
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointSearchEntry {
    pub id: String,
    pub agent: String,
    pub working_on: String,
    pub state: serde_json::Value,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
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

/// Clamp a requested result count to `1..=cap`.
///
/// `cap` is the configured maximum result limit when set, otherwise the
/// endpoint's built-in `default_cap`.
fn result_limit(state: &McpState, requested: i64, default_cap: i64) -> i64 {
    let cap = state.max_result_limit.map_or(default_cap, i64::from);
    requested.clamp(1, cap)
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
        .route("/api/v1/lessons/search", get(search_lessons))
        .route(
            "/api/v1/lessons/backfill-embeddings",
            post(backfill_lesson_embeddings),
        )
        .route(
            "/api/v1/lessons/{id}",
            get(get_lesson_by_id).delete(delete_lesson),
        )
        .route("/api/v1/metrics", get(tool_metrics))
        .route("/api/v1/search/hybrid", get(hybrid_search))
        .route(
            "/api/v1/checkpoints",
            get(list_checkpoints).post(create_checkpoint),
        )
        .route("/api/v1/checkpoints/search", get(search_checkpoints))
        .route(
            "/api/v1/checkpoints/backfill-embeddings",
            post(backfill_checkpoint_embeddings),
        )
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
    let limit = result_limit(&state, params.limit, 200) as usize;

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

    let limit = result_limit(&state, params.limit, 100);

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
    let limit = result_limit(&state, params.limit, 200) as usize;

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
        let embed_text = crate::embeddings::lesson_embedding_text(&lesson.title, &body.content);
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

/// GET /api/v1/lessons/:id - Fetch a lesson, following tombstones.
///
/// A live lesson is returned as 200. A deleted id with a live successor
/// returns 301 with `Location` set to the successor and the pointer in the
/// body; a deleted id with no live successor returns 410; an unknown id 404.
/// If several prefix tombstones match, 409 lists them.
async fn get_lesson_by_id(State(state): State<Arc<McpState>>, Path(id): Path<String>) -> Response {
    let resolution = match state
        .db()
        .with_conn(|conn| storage::resolve_lesson_id(conn, &id))
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, lesson_id = %id, "Failed to resolve lesson");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    match resolution {
        storage::LessonResolution::Live(l) => Json(LessonEntry {
            id: l.id,
            title: l.title,
            content: l.content,
            severity: l.severity,
            tags: l.tags,
            created_at: l.created_at,
        })
        .into_response(),
        storage::LessonResolution::Moved { chain, reason } => {
            let successor = chain.last().cloned().unwrap_or_default();
            let body = Json(serde_json::json!({
                "id": id,
                "status": "moved",
                "successor_id": successor,
                "chain": chain,
                "reason": reason,
            }));
            let mut response = (StatusCode::MOVED_PERMANENTLY, body).into_response();
            if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/lessons/{successor}")) {
                response.headers_mut().insert(header::LOCATION, location);
            }
            response
        }
        storage::LessonResolution::Deleted {
            chain,
            reason,
            broken,
        } => (
            StatusCode::GONE,
            Json(serde_json::json!({
                "id": id,
                "status": "deleted",
                "reason": reason,
                "chain": chain,
                "broken": broken,
            })),
        )
            .into_response(),
        storage::LessonResolution::Ambiguous(candidates) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "id": id,
                "status": "ambiguous",
                "candidates": candidates,
            })),
        )
            .into_response(),
        storage::LessonResolution::NotFound => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "id": id, "status": "not_found" })),
        )
            .into_response(),
    }
}

/// DELETE /api/v1/lessons/:id - Delete a lesson.
///
/// Records a tombstone for the id. With `?successor=<id>` the tombstone
/// points at that lesson, which must resolve to a live lesson (else 400 and
/// nothing is deleted); `?reason=<text>` is stored with it.
async fn delete_lesson(
    State(state): State<Arc<McpState>>,
    Path(id): Path<String>,
    Query(params): Query<DeleteLessonQuery>,
) -> Response {
    let successor = params.successor.as_deref().filter(|s| !s.is_empty());
    let reason = params.reason.as_deref().unwrap_or("");
    let outcome = state.db().with_transaction(|conn| {
        storage::delete_lesson_with_tombstone(conn, &id, successor, reason)
    });

    match outcome {
        Ok(storage::TombstoneDelete::Deleted { .. }) => {
            tracing::info!(id = %id, "Lesson deleted via UI");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(storage::TombstoneDelete::InvalidSuccessor) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "id": id,
                "error": "successor does not resolve to another live lesson",
                "successor": successor,
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, lesson_id = %id, "Failed to delete lesson");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/v1/lessons/search - Search over lessons.
///
/// Fuses keyword (BM25) and vector rankings when the embedding service is
/// available (`search_type: "hybrid"`); otherwise ranks by keywords alone,
/// falling back to a substring match if that finds nothing
/// (`search_type: "text"`). See [`storage::search_lessons_hybrid`] for what
/// `score` means.
async fn search_lessons(
    State(state): State<Arc<McpState>>,
    Query(params): Query<LessonSearchQuery>,
) -> Result<Json<LessonSearchResponse>, StatusCode> {
    if params.q.trim().is_empty() {
        return Ok(Json(LessonSearchResponse {
            lessons: Vec::new(),
            query: params.q,
            total: 0,
            search_type: "none".to_string(),
        }));
    }

    let limit = result_limit(
        &state,
        params.limit,
        i64::try_from(storage::LESSON_SEARCH_DEFAULT_CAP).unwrap_or(100),
    ) as usize;

    let mut query_embedding = None;
    if let Some(ref embedding_service) = state.embeddings {
        match embedding_service.embed_one(params.q.clone()).await {
            Ok(embedding) => query_embedding = Some(embedding),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Lesson embedding failed, falling back to keyword search"
                );
            }
        }
    }

    let results = state
        .db()
        .with_conn(|conn| {
            storage::search_lessons_hybrid(conn, &params.q, query_embedding.as_deref(), limit)
        })
        .map_err(|e| {
            tracing::error!(error = %e, "Lesson search failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let search_type = if query_embedding.is_some() {
        "hybrid"
    } else {
        "text"
    };

    let mut lessons: Vec<LessonSearchEntry> = results
        .into_iter()
        .map(|r| LessonSearchEntry {
            id: r.record.id,
            title: r.record.title,
            content: r.record.content,
            severity: r.record.severity,
            tags: r.record.tags,
            created_at: r.record.created_at,
            score: Some(r.score),
            similarity: r.similarity,
            keyword_rank: r.keyword_rank,
        })
        .collect();

    if lessons.is_empty() && query_embedding.is_none() {
        // Substring match catches partial words the tokenizer would not.
        lessons = state
            .db()
            .with_conn(|conn| storage::search_lessons_by_text(conn, &params.q, limit))
            .map_err(|e| {
                tracing::error!(error = %e, "Lesson text search failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .into_iter()
            .map(|l| LessonSearchEntry {
                id: l.id,
                title: l.title,
                content: l.content,
                severity: l.severity,
                tags: l.tags,
                created_at: l.created_at,
                score: None,
                similarity: None,
                keyword_rank: None,
            })
            .collect();
    }

    let total = lessons.len();
    Ok(Json(LessonSearchResponse {
        lessons,
        query: params.q,
        total,
        search_type: search_type.to_string(),
    }))
}

/// POST /api/v1/lessons/backfill-embeddings - Generate embeddings for lessons that lack them.
async fn backfill_lesson_embeddings(
    State(state): State<Arc<McpState>>,
) -> Result<Json<BackfillResponse>, StatusCode> {
    let embedding_service = state.embeddings.as_ref().ok_or_else(|| {
        tracing::error!("Backfill requested but embedding service not available");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let all_lessons = state.db().with_conn(storage::list_lessons).map_err(|e| {
        tracing::error!(error = %e, "Failed to list lessons for backfill");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut processed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for lesson in &all_lessons {
        // Check if embedding already exists by attempting a search with a dummy
        let has_embedding = state
            .db()
            .with_conn(|conn| {
                let table = storage::embedding_meta::active_tables(conn)?.lessons;
                let count: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE id = ?"),
                        [&lesson.id],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                Ok::<bool, crate::Error>(count > 0)
            })
            .unwrap_or(false);

        if has_embedding {
            skipped += 1;
            continue;
        }

        let embed_text = crate::embeddings::lesson_embedding_text(&lesson.title, &lesson.content);
        match embedding_service.embed_one(embed_text).await {
            Ok(embedding) => {
                let lesson_id = lesson.id.clone();
                match state.db().with_conn(move |conn| {
                    storage::store_lesson_embedding(conn, &lesson_id, &embedding)
                }) {
                    Ok(()) => processed += 1,
                    Err(e) => {
                        tracing::warn!(error = %e, lesson_id = %lesson.id, "Failed to store backfill embedding");
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, lesson_id = %lesson.id, "Failed to generate backfill embedding");
                failed += 1;
            }
        }
    }

    tracing::info!(
        processed,
        skipped,
        failed,
        total = all_lessons.len(),
        "Lesson embedding backfill complete"
    );

    Ok(Json(BackfillResponse {
        processed,
        skipped,
        failed,
    }))
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

    let limit = usize::try_from(result_limit(&state, params.limit, 100)).unwrap_or(100);
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
    let limit = usize::try_from(result_limit(&state, params.limit, 200)).unwrap_or(20);

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

/// POST /api/v1/checkpoints - Create a new checkpoint with optional embedding.
async fn create_checkpoint(
    State(state): State<Arc<McpState>>,
    Json(body): Json<CreateCheckpointRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let checkpoint = storage::CheckpointRecord::new(&body.agent, &body.working_on, body.state);
    let id = checkpoint.id.clone();

    state
        .db()
        .with_conn(|conn| storage::insert_checkpoint(conn, &checkpoint))
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to create checkpoint");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if let Some(ref embedding_service) = state.embeddings {
        if embedding_service.is_initialized() {
            let text_to_embed =
                crate::embeddings::checkpoint_embedding_text(&checkpoint.working_on);
            match embedding_service.embed_one(text_to_embed).await {
                Ok(embedding) => {
                    let _ = state.db().with_conn(|conn| {
                        storage::store_checkpoint_embedding(conn, &id, &embedding)
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to generate checkpoint embedding");
                }
            }
        }
    }

    tracing::info!(id = %id, agent = %checkpoint.agent, "Checkpoint created via REST");

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "status": "created" })),
    ))
}

/// GET /api/v1/checkpoints/search - Semantic search over checkpoints.
///
/// Uses embedding similarity when available, falls back to text search.
/// Supports optional `agent` filter.
async fn search_checkpoints(
    State(state): State<Arc<McpState>>,
    Query(params): Query<CheckpointSearchQuery>,
) -> Result<Json<CheckpointSearchResponse>, StatusCode> {
    if params.q.trim().is_empty() {
        return Ok(Json(CheckpointSearchResponse {
            checkpoints: Vec::new(),
            query: params.q,
            total: 0,
            search_type: "none".to_string(),
        }));
    }

    let limit = result_limit(&state, params.limit, 100) as usize;

    if let Some(ref embedding_service) = state.embeddings {
        if embedding_service.is_initialized() {
            match embedding_service.embed_one(params.q.clone()).await {
                Ok(query_embedding) => {
                    let results = state
                        .db()
                        .with_conn(|conn| {
                            storage::search_checkpoints_by_embedding(conn, &query_embedding, limit)
                        })
                        .map_err(|e| {
                            tracing::error!(error = %e, "Checkpoint semantic search failed");
                            StatusCode::INTERNAL_SERVER_ERROR
                        })?;

                    let checkpoints: Vec<CheckpointSearchEntry> =
                        if let Some(ref agent) = params.agent {
                            results
                                .into_iter()
                                .filter(|r| r.record.agent == *agent)
                                .map(|r| CheckpointSearchEntry {
                                    id: r.record.id,
                                    agent: r.record.agent,
                                    working_on: r.record.working_on,
                                    state: r.record.state,
                                    created_at: r.record.created_at,
                                    score: Some(r.score),
                                })
                                .collect()
                        } else {
                            results
                                .into_iter()
                                .map(|r| CheckpointSearchEntry {
                                    id: r.record.id,
                                    agent: r.record.agent,
                                    working_on: r.record.working_on,
                                    state: r.record.state,
                                    created_at: r.record.created_at,
                                    score: Some(r.score),
                                })
                                .collect()
                        };

                    let total = checkpoints.len();
                    return Ok(Json(CheckpointSearchResponse {
                        checkpoints,
                        query: params.q,
                        total,
                        search_type: "semantic".to_string(),
                    }));
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Checkpoint embedding failed, falling back to text search"
                    );
                }
            }
        }
    }

    let results = state
        .db()
        .with_conn(|conn| storage::search_checkpoints_by_text(conn, &params.q, limit))
        .map_err(|e| {
            tracing::error!(error = %e, "Checkpoint text search failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let checkpoints: Vec<CheckpointSearchEntry> = if let Some(ref agent) = params.agent {
        results
            .into_iter()
            .filter(|cp| cp.agent == *agent)
            .map(|cp| CheckpointSearchEntry {
                id: cp.id,
                agent: cp.agent,
                working_on: cp.working_on,
                state: cp.state,
                created_at: cp.created_at,
                score: None,
            })
            .collect()
    } else {
        results
            .into_iter()
            .map(|cp| CheckpointSearchEntry {
                id: cp.id,
                agent: cp.agent,
                working_on: cp.working_on,
                state: cp.state,
                created_at: cp.created_at,
                score: None,
            })
            .collect()
    };

    let total = checkpoints.len();
    Ok(Json(CheckpointSearchResponse {
        checkpoints,
        query: params.q,
        total,
        search_type: "text".to_string(),
    }))
}

/// POST /api/v1/checkpoints/backfill-embeddings - Generate embeddings for checkpoints that lack them.
async fn backfill_checkpoint_embeddings(
    State(state): State<Arc<McpState>>,
) -> Result<Json<BackfillResponse>, StatusCode> {
    let embedding_service = state.embeddings.as_ref().ok_or_else(|| {
        tracing::error!("Checkpoint backfill requested but embedding service not available");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let all_checkpoints = state
        .db()
        .with_conn(|conn| storage::get_recent_checkpoints_all(conn, 10_000))
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to list checkpoints for backfill");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut processed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for cp in &all_checkpoints {
        let has_embedding = state
            .db()
            .with_conn(|conn| {
                let table = storage::embedding_meta::active_tables(conn)?.checkpoints;
                let count: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE id = ?"),
                        [&cp.id],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                Ok::<bool, crate::Error>(count > 0)
            })
            .unwrap_or(false);

        if has_embedding {
            skipped += 1;
            continue;
        }

        match embedding_service
            .embed_one(crate::embeddings::checkpoint_embedding_text(&cp.working_on))
            .await
        {
            Ok(embedding) => {
                let cp_id = cp.id.clone();
                match state.db().with_conn(move |conn| {
                    storage::store_checkpoint_embedding(conn, &cp_id, &embedding)
                }) {
                    Ok(()) => processed += 1,
                    Err(e) => {
                        tracing::warn!(error = %e, checkpoint_id = %cp.id, "Failed to store checkpoint backfill embedding");
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, checkpoint_id = %cp.id, "Failed to generate checkpoint backfill embedding");
                failed += 1;
            }
        }
    }

    tracing::info!(
        processed,
        skipped,
        failed,
        total = all_checkpoints.len(),
        "Checkpoint embedding backfill complete"
    );

    Ok(Json(BackfillResponse {
        processed,
        skipped,
        failed,
    }))
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
    let limit = usize::try_from(result_limit(&state, params.limit, 500)).unwrap_or(50);

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

    fn insert_lesson_with_id(state: &McpState, id: &str) {
        let mut lesson = storage::LessonRecord::new(format!("Lesson {id}"), "Body", vec![]);
        lesson.id = id.to_string();
        state
            .db()
            .with_conn(|conn| storage::insert_lesson(conn, &lesson))
            .unwrap();
    }

    fn tombstone(state: &McpState, old_id: &str, successor: Option<&str>, reason: &str) {
        state
            .db()
            .with_conn(|conn| storage::upsert_tombstone(conn, old_id, successor, reason))
            .unwrap();
    }

    async fn send(
        state: &Arc<McpState>,
        method: &str,
        uri: &str,
    ) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let response = create_api_router(Arc::clone(state))
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        (status, headers, json)
    }

    #[tokio::test]
    async fn test_get_lesson_live() {
        let state = create_test_state();
        insert_lesson_with_id(&state, "live-lesson-1");
        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/live-lesson-1").await;
        assert_eq!(status, StatusCode::OK);
        let lesson: LessonEntry = serde_json::from_value(body).unwrap();
        assert_eq!(lesson.id, "live-lesson-1");
        assert_eq!(lesson.title, "Lesson live-lesson-1");
    }

    #[tokio::test]
    async fn test_get_lesson_moved_follows_chain() {
        let state = create_test_state();
        insert_lesson_with_id(&state, "lesson-final");
        tombstone(&state, "lesson-first", Some("lesson-middle"), "retitled");
        tombstone(&state, "lesson-middle", Some("lesson-final"), "folded");
        let (status, headers, body) = send(&state, "GET", "/api/v1/lessons/lesson-first").await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(
            headers.get(header::LOCATION).unwrap(),
            "/api/v1/lessons/lesson-final"
        );
        assert_eq!(body["id"], "lesson-first");
        assert_eq!(body["status"], "moved");
        assert_eq!(body["successor_id"], "lesson-final");
        assert_eq!(
            body["chain"],
            serde_json::json!(["lesson-middle", "lesson-final"])
        );
        assert_eq!(body["reason"], "retitled");
    }

    #[tokio::test]
    async fn test_get_lesson_deleted_unknown_and_cycle() {
        let state = create_test_state();
        tombstone(&state, "lesson-gone", None, "obsolete");
        tombstone(&state, "loop-one", Some("loop-two"), "");
        tombstone(&state, "loop-two", Some("loop-one"), "");

        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/lesson-gone").await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["status"], "deleted");
        assert_eq!(body["reason"], "obsolete");

        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/loop-one").await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["broken"], "cycle");

        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/never-existed").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            serde_json::json!({"id": "never-existed", "status": "not_found"})
        );
    }

    #[tokio::test]
    async fn test_get_lesson_prefix_and_ambiguous_prefix() {
        let state = create_test_state();
        insert_lesson_with_id(&state, "replacement");
        tombstone(&state, "0a1b2c3d", Some("replacement"), "short id");
        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/0a1b2c3d-full-id").await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(body["successor_id"], "replacement");

        tombstone(&state, "9f8e7d6c", None, "");
        tombstone(&state, "9f8e7d6c5", None, "");
        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/9f8e7d6c5b-full-id").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["status"], "ambiguous");
        assert_eq!(body["candidates"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_deleted_lesson_leaves_search_and_tombstone_holds_no_content() {
        let state = create_test_state();
        let mut lesson = storage::LessonRecord::new(
            "Quokka zebra secret",
            "token-like body text that must not survive",
            vec![],
        );
        lesson.id = "purged".to_string();
        state
            .db()
            .with_conn(|conn| storage::insert_lesson(conn, &lesson))
            .unwrap();

        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/search?q=Quokka").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 1);

        let (status, _, _) = send(&state, "DELETE", "/api/v1/lessons/purged?reason=purged").await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Gone from search, answered as deleted by GET.
        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/search?q=Quokka").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 0);
        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/purged").await;
        assert_eq!(status, StatusCode::GONE);
        assert!(!body.to_string().contains("Quokka"));
        assert!(!body.to_string().contains("token-like"));

        // The tombstone stores only ids, the caller's reason and a time.
        let columns: Vec<String> = state
            .db()
            .with_conn(|conn| {
                let mut stmt = conn
                    .prepare("SELECT name FROM pragma_table_info('lesson_tombstones') ORDER BY cid")
                    .map_err(|e| crate::Error::internal(e.to_string()))?;
                let names = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| crate::Error::internal(e.to_string()))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| crate::Error::internal(e.to_string()))?;
                Ok(names)
            })
            .unwrap();
        assert_eq!(columns, ["old_id", "successor_id", "reason", "created_at"]);
    }

    #[tokio::test]
    async fn test_delete_lesson_records_tombstone() {
        let state = create_test_state();
        insert_lesson_with_id(&state, "to-retitle");
        insert_lesson_with_id(&state, "retitled-copy");
        insert_lesson_with_id(&state, "to-drop");

        let (status, _, body) = send(
            &state,
            "DELETE",
            "/api/v1/lessons/to-retitle?successor=does-not-exist",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["id"], "to-retitle");
        let (status, _, _) = send(&state, "GET", "/api/v1/lessons/to-retitle").await;
        assert_eq!(status, StatusCode::OK);

        let (status, _, _) = send(
            &state,
            "DELETE",
            "/api/v1/lessons/to-retitle?successor=retitled-copy&reason=new%20title",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, headers, body) = send(&state, "GET", "/api/v1/lessons/to-retitle").await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(
            headers.get(header::LOCATION).unwrap(),
            "/api/v1/lessons/retitled-copy"
        );
        assert_eq!(body["reason"], "new title");

        let (status, _, _) = send(&state, "DELETE", "/api/v1/lessons/to-drop").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, body) = send(&state, "GET", "/api/v1/lessons/to-drop").await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["status"], "deleted");
    }

    #[tokio::test]
    async fn test_search_lessons_empty_query() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/lessons/search?q=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: LessonSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 0);
        assert_eq!(result.search_type, "none");
    }

    #[tokio::test]
    async fn test_search_lessons_text_fallback() {
        let state = create_test_state();

        // Insert a lesson
        state
            .db()
            .with_conn(|conn| {
                let lesson = storage::LessonRecord::new(
                    "Rust Error Handling",
                    "Use Result type for error handling in Rust",
                    vec!["rust".to_string()],
                );
                storage::insert_lesson(conn, &lesson)
            })
            .unwrap();

        let app = create_api_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/lessons/search?q=Rust")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: LessonSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.search_type, "text");
        assert!(result.lessons[0].title.contains("Rust"));
        // Ranked by keywords alone: first in one of two possible rankings.
        assert_eq!(result.lessons[0].score, Some(0.5));
        assert_eq!(result.lessons[0].similarity, None);
        assert_eq!(result.lessons[0].keyword_rank, Some(1));

        // A partial word the full-text index cannot match falls back to a
        // substring match, which has no score.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/lessons/search?q=Rus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: LessonSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.search_type, "text");
        assert!(result.lessons[0].score.is_none());
    }

    #[tokio::test]
    async fn test_backfill_no_embeddings() {
        let state = create_test_state();
        let app = create_api_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/lessons/backfill-embeddings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // No embedding service = 503
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
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

    fn create_test_state_with_max(max: Option<u32>) -> Arc<McpState> {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| migrate(conn)).unwrap();
        let mut state = McpState::new(db);
        state.set_max_result_limit(max);
        Arc::new(state)
    }

    #[test]
    fn test_result_limit_default_caps_unchanged() {
        let state = create_test_state_with_max(None);
        for cap in [100, 200, 500] {
            assert_eq!(result_limit(&state, 0, cap), 1);
            assert_eq!(result_limit(&state, -7, cap), 1);
            assert_eq!(result_limit(&state, 20, cap), 20);
            assert_eq!(result_limit(&state, cap, cap), cap);
            assert_eq!(result_limit(&state, 10_000, cap), cap);
        }
    }

    #[test]
    fn test_result_limit_override() {
        let state = create_test_state_with_max(Some(10_000));
        for cap in [100, 200, 500] {
            assert_eq!(result_limit(&state, 0, cap), 1);
            assert_eq!(result_limit(&state, 20, cap), 20);
            assert_eq!(result_limit(&state, 5_000, cap), 5_000);
            assert_eq!(result_limit(&state, 50_000, cap), 10_000);
        }

        // A configured value below a built-in cap lowers it too.
        let state = create_test_state_with_max(Some(50));
        assert_eq!(result_limit(&state, 500, 500), 50);
    }

    async fn list_lessons_count(state: Arc<McpState>, limit: i64) -> usize {
        let response = create_api_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/lessons?limit={limit}&offset=0"))
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
        lessons.lessons.len()
    }

    fn insert_lessons(state: &McpState, n: usize) {
        state
            .db()
            .with_conn(|conn| {
                for i in 0..n {
                    let lesson = storage::LessonRecord::new(
                        format!("Lesson {i}"),
                        "content",
                        vec!["test".to_string()],
                    );
                    storage::insert_lesson(conn, &lesson)?;
                }
                Ok(())
            })
            .unwrap();
    }

    #[tokio::test]
    async fn test_list_lessons_limit_default_cap() {
        let state = create_test_state_with_max(None);
        insert_lessons(&state, 250);
        assert_eq!(list_lessons_count(state, 1_000).await, 200);
    }

    #[tokio::test]
    async fn test_list_lessons_limit_override() {
        let state = create_test_state_with_max(Some(10_000));
        insert_lessons(&state, 250);
        assert_eq!(list_lessons_count(Arc::clone(&state), 1_000).await, 250);
        assert_eq!(list_lessons_count(state, 230).await, 230);
    }
}
