//! Embedded web UI serving.
//!
//! Serves the dashboard HTML, CSS, and JS files that are embedded
//! into the binary at compile time via `include_str!`.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};

// Embed static files at compile time
const INDEX_HTML: &str = include_str!("../../static/index.html");
const SEARCH_HTML: &str = include_str!("../../static/search.html");
const LESSONS_HTML: &str = include_str!("../../static/lessons.html");
const FILES_HTML: &str = include_str!("../../static/files.html");
const METRICS_HTML: &str = include_str!("../../static/metrics.html");
const CHECKPOINTS_HTML: &str = include_str!("../../static/checkpoints.html");
const GRAPH_HTML: &str = include_str!("../../static/graph.html");
const STYLE_CSS: &str = include_str!("../../static/style.css");

/// Create the UI router.
pub fn create_ui_router() -> Router {
    Router::new()
        .route("/ui", get(dashboard_page))
        .route("/ui/search", get(search_page))
        .route("/ui/lessons", get(lessons_page))
        .route("/ui/files", get(files_page))
        .route("/ui/metrics", get(metrics_page))
        .route("/ui/checkpoints", get(checkpoints_page))
        .route("/ui/graph", get(graph_page))
        .route("/ui/static/{*path}", get(static_file))
}

/// GET /ui - Dashboard page.
async fn dashboard_page() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// GET /ui/search - Search page.
async fn search_page() -> Html<&'static str> {
    Html(SEARCH_HTML)
}

/// GET /ui/lessons - Lessons page.
async fn lessons_page() -> Html<&'static str> {
    Html(LESSONS_HTML)
}

/// GET /ui/files - Files page.
async fn files_page() -> Html<&'static str> {
    Html(FILES_HTML)
}

/// GET /ui/metrics - Tool metrics page.
async fn metrics_page() -> Html<&'static str> {
    Html(METRICS_HTML)
}

/// GET /ui/checkpoints - Checkpoint browser page.
async fn checkpoints_page() -> Html<&'static str> {
    Html(CHECKPOINTS_HTML)
}

/// GET /ui/graph - Knowledge graph explorer page.
async fn graph_page() -> Html<&'static str> {
    Html(GRAPH_HTML)
}

/// GET /ui/static/* - Serve static assets (CSS, JS).
async fn static_file(Path(path): Path<String>) -> impl IntoResponse {
    match path.as_str() {
        "style.css" => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            STYLE_CSS,
        ),
        _ => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "Not found",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_dashboard_page() {
        let app = create_ui_router();
        let response = app
            .oneshot(Request::builder().uri("/ui").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Nellie Dashboard"));
        assert!(html.contains("htmx"));
    }

    #[tokio::test]
    async fn test_search_page() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Code Search"));
    }

    #[tokio::test]
    async fn test_lessons_page() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/lessons")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_files_page() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/files")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics_page() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Tool Metrics"));
        assert!(html.contains("/api/v1/metrics"));
        assert!(html.contains("htmx"));
    }

    #[tokio::test]
    async fn test_checkpoints_page() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/checkpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Checkpoint Browser"));
        assert!(html.contains("/api/v1/checkpoints"));
        assert!(html.contains("/api/v1/agents"));
        assert!(html.contains("htmx"));
    }

    #[tokio::test]
    async fn test_graph_page() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Knowledge Graph Explorer"));
        assert!(html.contains("/api/v1/graph"));
        assert!(html.contains("vis-network"));
    }

    #[tokio::test]
    async fn test_static_css() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/static/style.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/css"));
    }

    #[tokio::test]
    async fn test_static_not_found() {
        let app = create_ui_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui/static/nonexistent.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
