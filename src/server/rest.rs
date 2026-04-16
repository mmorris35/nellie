//! REST API endpoints.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use prometheus::{Encoder, TextEncoder};
use serde::Serialize;

use super::mcp::McpState;

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub database: String,
}

/// Create REST API router.
pub fn create_rest_router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(metrics))
        .route("/api/v1/status", get(status))
        .with_state(state)
}

/// Health check endpoint.
async fn health_check(State(state): State<Arc<McpState>>) -> impl IntoResponse {
    let db_status = match state.db.health_check() {
        Ok(()) => "ok",
        Err(e) => {
            tracing::warn!(error = %e, "Database health check failed");
            "error"
        }
    };

    let response = HealthResponse {
        status: if db_status == "ok" {
            "healthy"
        } else {
            "unhealthy"
        }
        .to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        database: db_status.to_string(),
    };

    let status_code = if db_status == "ok" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    tracing::debug!(status = ?status_code, database = %db_status, "Health check");

    (status_code, Json(response))
}

/// Prometheus metrics endpoint.
async fn metrics(State(_state): State<Arc<McpState>>) -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();

    let mut buffer = Vec::new();
    match encoder.encode(&metric_families, &mut buffer) {
        Ok(()) => {
            tracing::trace!("Metrics encoded successfully");
            (
                StatusCode::OK,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                buffer,
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to encode metrics");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                b"Failed to encode metrics".to_vec(),
            )
        }
    }
}

/// Status endpoint with statistics.
async fn status(State(state): State<Arc<McpState>>) -> impl IntoResponse {
    let chunk_count = state
        .db
        .with_conn(crate::storage::count_chunks)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to count chunks");
            0
        });

    let lesson_count = state
        .db
        .with_conn(crate::storage::count_lessons)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to count lessons");
            0
        });

    let file_count = state
        .db
        .with_conn(crate::storage::count_tracked_files)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to count tracked files");
            0
        });

    tracing::debug!(
        chunks = chunk_count,
        lessons = lesson_count,
        files = file_count,
        "Status retrieved"
    );

    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "stats": {
            "indexed_chunks": chunk_count,
            "lessons": lesson_count,
            "tracked_files": file_count
        }
    }))
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
    async fn test_health_check() {
        let state = create_test_state();
        let app = create_rest_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics() {
        let state = create_test_state();
        let app = create_rest_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics_include_tool_observability() {
        use crate::server::metrics;

        let state = create_test_state();

        // Simulate a tool call
        metrics::record_tool_call(
            "search_code",
            "user/test-agent",
            "success",
            std::time::Duration::from_millis(150),
            2048,
        );

        let app = create_rest_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();

        // Verify tool invocation counter
        assert!(
            body_str.contains("nellie_tool_invocations_total"),
            "metrics should contain nellie_tool_invocations_total"
        );
        // Verify tool latency histogram
        assert!(
            body_str.contains("nellie_tool_duration_seconds"),
            "metrics should contain nellie_tool_duration_seconds"
        );
        // Verify response bytes counter
        assert!(
            body_str.contains("nellie_tool_response_bytes_total"),
            "metrics should contain nellie_tool_response_bytes_total"
        );
        // Verify token savings counter
        assert!(
            body_str.contains("nellie_estimated_tokens_saved_total"),
            "metrics should contain nellie_estimated_tokens_saved_total"
        );
        // Verify agent label is present
        assert!(
            body_str.contains("user/test-agent"),
            "metrics should contain agent label value"
        );
        // Verify tool label is present
        assert!(
            body_str.contains("search_code"),
            "metrics should contain tool label value"
        );
    }

    #[tokio::test]
    async fn test_status() {
        let state = create_test_state();
        let app = create_rest_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
