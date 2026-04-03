//! MCP and REST API servers.
//!
//! This module provides:
//! - MCP server using rmcp with StreamableHttpService
//! - REST API using axum
//! - Health and metrics endpoints
//! - API key authentication middleware
//! - Graceful shutdown coordination
//! - Structured logging and tracing observability

#[allow(clippy::option_if_let_else)]
mod api;
mod app;
mod auth;
#[allow(
    clippy::option_if_let_else,
    clippy::significant_drop_tightening,
    clippy::ignored_unit_patterns
)]
mod mcp;
#[allow(
    clippy::option_if_let_else,
    clippy::ignored_unit_patterns,
    clippy::manual_async_fn,
    clippy::redundant_closure
)]
mod mcp_transport;
mod metrics;
pub mod observability;
mod rest;
mod sse;
mod ui;

pub use api::create_api_router;
pub use app::{App, ServerConfig};
pub use auth::ApiKeyConfig;
pub use mcp::{create_mcp_router, get_tools, McpState, ToolRequest, ToolResponse};
pub use mcp_transport::{start_mcp_server, McpTransportConfig, NellieMcpHandler};
pub use metrics::{init_metrics, CHUNKS_TOTAL, EMBEDDING_QUEUE_DEPTH, FILES_TOTAL, LESSONS_TOTAL};
pub use observability::init_tracing;
pub use rest::{create_rest_router, HealthResponse};
pub use sse::create_sse_router;
pub use ui::create_ui_router;

/// Initialize server module.
pub fn init() {
    ::tracing::debug!("Server module initialized");
}
