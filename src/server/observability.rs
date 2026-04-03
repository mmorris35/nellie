//! Structured logging and tracing configuration.
//!
//! Provides setup for observability using the `tracing` crate with:
//! - Structured logging with JSON output option
//! - Request tracing middleware integration
//! - Configurable log levels
//! - Span propagation for distributed tracing

use tracing_subscriber::{
    filter::EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt, Registry,
};

/// Tracing configuration options.
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// Log level (trace, debug, info, warn, error)
    pub level: String,
    /// Enable JSON output format
    pub json: bool,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            json: false,
        }
    }
}

/// Initialize tracing with the given configuration.
///
/// Sets up the tracing subscriber with:
/// - Configured log level from environment or config
/// - Structured logging output (plain text or JSON)
/// - Request tracing spans
/// - Proper error handling
///
/// # Arguments
///
/// * `config` - Tracing configuration options
///
/// # Panics
///
/// Panics if tracing subscriber has already been initialized in this process.
pub fn init_tracing(level: &str, json: bool) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    if json {
        let json_layer = fmt::layer()
            .json()
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_file(true)
            .with_line_number(true);

        Registry::default().with(env_filter).with(json_layer).init();
    } else {
        let fmt_layer = fmt::layer()
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_file(true)
            .with_line_number(true);

        Registry::default().with(env_filter).with(fmt_layer).init();
    }

    tracing::debug!("Tracing initialized: level={}, json={}", level, json);
}

/// Get current tracing configuration from environment variables.
///
/// Respects these environment variables:
/// - `NELLIE_LOG_LEVEL` - Log level (default: "info")
/// - `NELLIE_LOG_JSON` - Enable JSON output (default: false)
///
/// # Returns
///
/// A `TracingConfig` with values from environment or defaults
#[must_use]
pub fn config_from_env() -> TracingConfig {
    let level = std::env::var("NELLIE_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let json = std::env::var("NELLIE_LOG_JSON")
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(false);

    TracingConfig { level, json }
}

/// Span context for distributed tracing.
///
/// Provides utilities for working with request spans and
/// propagating context across async boundaries.
pub mod spans {
    use tracing::{info_span, Span};

    /// Create a new request span with common fields.
    ///
    /// # Arguments
    ///
    /// * `method` - HTTP method (GET, POST, etc.)
    /// * `uri` - Request URI path
    /// * `request_id` - Unique request identifier
    ///
    /// # Returns
    ///
    /// A new tracing span for this request
    #[must_use]
    pub fn request_span(method: &str, uri: &str, request_id: &str) -> Span {
        info_span!(
            "request",
            method = %method,
            uri = %uri,
            request_id = %request_id,
        )
    }

    /// Create a span for a tool invocation.
    ///
    /// # Arguments
    ///
    /// * `tool_name` - Name of the tool being invoked
    ///
    /// # Returns
    ///
    /// A new tracing span for this tool call
    #[must_use]
    pub fn tool_span(tool_name: &str) -> Span {
        info_span!(
            "tool_invocation",
            tool = %tool_name,
        )
    }

    /// Create a span for a database operation.
    ///
    /// # Arguments
    ///
    /// * `operation` - Type of operation (query, insert, update, etc.)
    /// * `table` - Database table name
    ///
    /// # Returns
    ///
    /// A new tracing span for this database operation
    #[must_use]
    pub fn db_span(operation: &str, table: &str) -> Span {
        info_span!(
            "db_operation",
            operation = %operation,
            table = %table,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracing_config_default() {
        let config = TracingConfig::default();
        assert_eq!(config.level, "info");
        assert!(!config.json);
    }

    #[test]
    fn test_tracing_config_custom() {
        let config = TracingConfig {
            level: "debug".to_string(),
            json: true,
        };
        assert_eq!(config.level, "debug");
        assert!(config.json);
    }

    #[test]
    fn test_span_creation() {
        let span = spans::request_span("GET", "/health", "req-123");
        let _guard = span.enter();
        // Span successfully created and entered
        assert!(true);
    }

    #[test]
    fn test_tool_span() {
        let span = spans::tool_span("search_code");
        let _guard = span.enter();
        assert!(true);
    }

    #[test]
    fn test_db_span() {
        let span = spans::db_span("select", "chunks");
        let _guard = span.enter();
        assert!(true);
    }
}
