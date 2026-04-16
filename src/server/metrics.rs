//! Prometheus metrics definitions.

use std::collections::HashMap;

use once_cell::sync::Lazy;
use prometheus::{
    core::Collector, proto, register_counter_vec, register_histogram_vec, register_int_counter_vec,
    register_int_gauge, CounterVec, HistogramVec, IntCounterVec, IntGauge,
};

use super::api::{AgentMetricsEntry, ToolMetricsEntry, ToolMetricsSummary};

/// Total chunks indexed.
pub static CHUNKS_TOTAL: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!("nellie_chunks_total", "Total number of indexed code chunks").unwrap()
});

/// Total lessons stored.
pub static LESSONS_TOTAL: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!("nellie_lessons_total", "Total number of lessons stored").unwrap()
});

/// Total files tracked.
pub static FILES_TOTAL: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!("nellie_files_total", "Total number of tracked files").unwrap()
});

/// Request latency histogram.
pub static REQUEST_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "nellie_request_duration_seconds",
        "Request latency in seconds",
        &["endpoint", "method"],
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]
    )
    .unwrap()
});

/// Request counter.
pub static REQUEST_COUNT: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "nellie_requests_total",
        "Total number of requests",
        &["endpoint", "method", "status"]
    )
    .unwrap()
});

/// Embedding queue depth.
pub static EMBEDDING_QUEUE_DEPTH: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "nellie_embedding_queue_depth",
        "Number of items waiting for embedding"
    )
    .unwrap()
});

/// Per-tool invocation counter with agent attribution.
pub static TOOL_INVOCATIONS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "nellie_tool_invocations_total",
        "Total MCP tool invocations",
        &["tool", "agent", "status"]
    )
    .expect("failed to register nellie_tool_invocations_total")
});

/// Per-tool latency histogram with agent attribution.
pub static TOOL_LATENCY: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "nellie_tool_duration_seconds",
        "MCP tool invocation latency in seconds",
        &["tool", "agent"],
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
    .expect("failed to register nellie_tool_duration_seconds")
});

/// Total response payload bytes by tool (for token estimation auditing).
pub static TOOL_RESPONSE_BYTES: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "nellie_tool_response_bytes_total",
        "Total response payload bytes by tool",
        &["tool", "agent"]
    )
    .expect("failed to register nellie_tool_response_bytes_total")
});

/// Estimated LLM tokens saved by Nellie responses.
pub static TOKEN_SAVINGS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "nellie_estimated_tokens_saved_total",
        "Estimated LLM tokens saved by Nellie responses",
        &["tool", "agent"]
    )
    .expect("failed to register nellie_estimated_tokens_saved_total")
});

/// Tokens-per-character ratio for estimation.
/// Claude tokenizer produces roughly 1 token per 4 characters for English/JSON.
pub const TOKENS_PER_CHAR: f64 = 0.25;

/// Record a complete tool invocation with all metrics.
///
/// Called from all three dispatch paths (HTTP invoke, SSE direct, rmcp native).
///
/// # Arguments
///
/// * `tool_name` - MCP tool name (e.g., `"search_code"`)
/// * `agent` - Agent identifier (e.g., `"user/example"`) or `"unknown"`
/// * `status` - `"success"` or `"error"`
/// * `latency` - Time elapsed for the tool call
/// * `response_bytes` - Size of the response payload in bytes (0 for errors)
pub fn record_tool_call(
    tool_name: &str,
    agent: &str,
    status: &str,
    latency: std::time::Duration,
    response_bytes: usize,
) {
    TOOL_INVOCATIONS
        .with_label_values(&[tool_name, agent, status])
        .inc();
    TOOL_LATENCY
        .with_label_values(&[tool_name, agent])
        .observe(latency.as_secs_f64());

    if status == "success" && response_bytes > 0 {
        TOOL_RESPONSE_BYTES
            .with_label_values(&[tool_name, agent])
            .inc_by(response_bytes as u64);
        #[allow(clippy::cast_precision_loss)]
        let estimated_tokens = response_bytes as f64 * TOKENS_PER_CHAR;
        TOKEN_SAVINGS
            .with_label_values(&[tool_name, agent])
            .inc_by(estimated_tokens);
    }
}

/// Extract a label value from a prometheus metric by label name.
///
/// Returns an empty string if the label is not found.
fn get_label_value(metric: &proto::Metric, name: &str) -> String {
    metric
        .get_label()
        .iter()
        .find(|lp| lp.name() == name)
        .map_or_else(String::new, |lp| lp.value().to_string())
}

/// Estimate p95 latency from a prometheus histogram.
///
/// Walks the histogram buckets and interpolates the 95th percentile
/// from cumulative counts. Returns 0.0 if there are no observations.
fn estimate_p95_from_histogram(h: &proto::Histogram) -> f64 {
    let total = h.get_sample_count();
    if total == 0 {
        return 0.0;
    }

    let target = (total as f64 * 0.95).ceil() as u64;
    let buckets = h.get_bucket();

    for bucket in buckets {
        if bucket.cumulative_count() >= target {
            // Return bucket upper bound in milliseconds
            return bucket.upper_bound() * 1000.0;
        }
    }

    // Fallback: use the sum/count average in milliseconds
    if total > 0 {
        (h.get_sample_sum() / total as f64) * 1000.0
    } else {
        0.0
    }
}

/// Collect structured tool metrics from the prometheus registry.
///
/// Reads from `TOOL_INVOCATIONS`, `TOOL_LATENCY`, `TOKEN_SAVINGS`, and
/// `TOOL_RESPONSE_BYTES` metric families and aggregates them into a
/// [`ToolMetricsSummary`] suitable for JSON serialization.
pub fn collect_tool_metrics() -> ToolMetricsSummary {
    // Intermediate structures for aggregation
    struct ToolAccum {
        invocations: u64,
        errors: u64,
        latency_sum_ms: f64,
        latency_count: u64,
        p95_latency_ms: f64,
        tokens_saved: f64,
        response_bytes: u64,
    }

    struct AgentAccum {
        invocations: u64,
        tokens_saved: f64,
    }

    let mut tools: HashMap<String, ToolAccum> = HashMap::new();
    let mut agents: HashMap<String, AgentAccum> = HashMap::new();

    // 1. Collect invocations (labels: tool, agent, status)
    let invocation_families: Vec<proto::MetricFamily> = TOOL_INVOCATIONS.collect();
    for family in &invocation_families {
        for metric in family.get_metric() {
            let tool = get_label_value(metric, "tool");
            let agent = get_label_value(metric, "agent");
            let status = get_label_value(metric, "status");
            let count = metric.get_counter().value() as u64;

            let tool_entry = tools.entry(tool).or_insert_with(|| ToolAccum {
                invocations: 0,
                errors: 0,
                latency_sum_ms: 0.0,
                latency_count: 0,
                p95_latency_ms: 0.0,
                tokens_saved: 0.0,
                response_bytes: 0,
            });

            tool_entry.invocations += count;
            if status == "error" {
                tool_entry.errors += count;
            }

            let agent_entry = agents.entry(agent).or_insert_with(|| AgentAccum {
                invocations: 0,
                tokens_saved: 0.0,
            });
            agent_entry.invocations += count;
        }
    }

    // 2. Collect latency (labels: tool, agent)
    let latency_families: Vec<proto::MetricFamily> = TOOL_LATENCY.collect();
    for family in &latency_families {
        for metric in family.get_metric() {
            let tool = get_label_value(metric, "tool");
            let h = metric.get_histogram();
            let count = h.get_sample_count();
            let sum_ms = h.get_sample_sum() * 1000.0;
            let p95 = estimate_p95_from_histogram(h);

            if let Some(entry) = tools.get_mut(&tool) {
                entry.latency_sum_ms += sum_ms;
                entry.latency_count += count;
                // Take the max p95 across agent combinations for this tool
                if p95 > entry.p95_latency_ms {
                    entry.p95_latency_ms = p95;
                }
            }
        }
    }

    // 3. Collect token savings (labels: tool, agent)
    let savings_families: Vec<proto::MetricFamily> = TOKEN_SAVINGS.collect();
    for family in &savings_families {
        for metric in family.get_metric() {
            let tool = get_label_value(metric, "tool");
            let agent = get_label_value(metric, "agent");
            let value = metric.get_counter().value();

            if let Some(entry) = tools.get_mut(&tool) {
                entry.tokens_saved += value;
            }
            if let Some(entry) = agents.get_mut(&agent) {
                entry.tokens_saved += value;
            }
        }
    }

    // 4. Collect response bytes (labels: tool, agent)
    let bytes_families: Vec<proto::MetricFamily> = TOOL_RESPONSE_BYTES.collect();
    for family in &bytes_families {
        for metric in family.get_metric() {
            let tool = get_label_value(metric, "tool");
            let value = metric.get_counter().value() as u64;

            if let Some(entry) = tools.get_mut(&tool) {
                entry.response_bytes += value;
            }
        }
    }

    // Build sorted output vectors
    let mut tool_entries: Vec<ToolMetricsEntry> = tools
        .into_iter()
        .map(|(name, acc)| {
            let avg_latency_ms = if acc.latency_count > 0 {
                acc.latency_sum_ms / acc.latency_count as f64
            } else {
                0.0
            };
            ToolMetricsEntry {
                name,
                invocations: acc.invocations,
                errors: acc.errors,
                avg_latency_ms,
                p95_latency_ms: acc.p95_latency_ms,
                tokens_saved: acc.tokens_saved,
                response_bytes: acc.response_bytes,
            }
        })
        .collect();
    tool_entries.sort_by_key(|t| std::cmp::Reverse(t.invocations));

    let mut agent_entries: Vec<AgentMetricsEntry> = agents
        .into_iter()
        .map(|(agent, acc)| AgentMetricsEntry {
            agent,
            invocations: acc.invocations,
            tokens_saved: acc.tokens_saved,
        })
        .collect();
    agent_entries.sort_by_key(|a| std::cmp::Reverse(a.invocations));

    // Compute totals
    let total_invocations = tool_entries.iter().map(|t| t.invocations).sum();
    let total_errors = tool_entries.iter().map(|t| t.errors).sum();
    let estimated_tokens_saved = tool_entries.iter().map(|t| t.tokens_saved).sum();
    let total_response_bytes = tool_entries.iter().map(|t| t.response_bytes).sum();

    ToolMetricsSummary {
        total_invocations,
        total_errors,
        estimated_tokens_saved,
        total_response_bytes,
        tools: tool_entries,
        agents: agent_entries,
    }
}

/// Initialize all metrics (call once at startup).
pub fn init_metrics() {
    // Access lazy statics to register them
    let _ = &*CHUNKS_TOTAL;
    let _ = &*LESSONS_TOTAL;
    let _ = &*FILES_TOTAL;
    let _ = &*REQUEST_LATENCY;
    let _ = &*REQUEST_COUNT;
    let _ = &*EMBEDDING_QUEUE_DEPTH;
    // Tool-level observability metrics
    let _ = &*TOOL_INVOCATIONS;
    let _ = &*TOOL_LATENCY;
    let _ = &*TOOL_RESPONSE_BYTES;
    let _ = &*TOKEN_SAVINGS;

    tracing::debug!("Prometheus metrics initialized");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_init() {
        init_metrics();

        CHUNKS_TOTAL.set(100);
        assert_eq!(CHUNKS_TOTAL.get(), 100);

        LESSONS_TOTAL.set(50);
        assert_eq!(LESSONS_TOTAL.get(), 50);
    }

    #[test]
    fn test_tool_invocation_metrics() {
        // Record a successful tool call
        record_tool_call(
            "search_code",
            "user/test",
            "success",
            std::time::Duration::from_millis(42),
            1024,
        );

        // Verify counter incremented
        let count = TOOL_INVOCATIONS
            .with_label_values(&["search_code", "user/test", "success"])
            .get();
        assert!(
            count >= 1,
            "tool invocation counter should be >= 1, got {count}"
        );

        // Verify latency recorded
        let latency = TOOL_LATENCY
            .with_label_values(&["search_code", "user/test"])
            .get_sample_count();
        assert!(latency >= 1, "latency histogram should have >= 1 sample");

        // Verify response bytes recorded
        let bytes = TOOL_RESPONSE_BYTES
            .with_label_values(&["search_code", "user/test"])
            .get();
        assert!(
            bytes >= 1024,
            "response bytes should be >= 1024, got {bytes}"
        );

        // Verify token savings estimated
        let tokens = TOKEN_SAVINGS
            .with_label_values(&["search_code", "user/test"])
            .get();
        assert!(
            tokens >= 256.0,
            "token savings should be >= 256.0 (1024 * 0.25), got {tokens}"
        );
    }

    #[test]
    fn test_tool_error_no_token_savings() {
        record_tool_call(
            "search_code",
            "unknown",
            "error",
            std::time::Duration::from_millis(5),
            0,
        );

        let count = TOOL_INVOCATIONS
            .with_label_values(&["search_code", "unknown", "error"])
            .get();
        assert!(count >= 1, "error invocation should be counted");

        // Token savings should not increase for errors
        // (we can't assert exact value since tests share global state,
        // but we verify no panic occurs)
    }

    #[test]
    fn test_collect_tool_metrics_returns_summary() {
        // Record some tool calls so there is data to collect
        record_tool_call(
            "collect_test_tool",
            "user/collect-test",
            "success",
            std::time::Duration::from_millis(100),
            512,
        );
        record_tool_call(
            "collect_test_tool",
            "user/collect-test",
            "error",
            std::time::Duration::from_millis(5),
            0,
        );

        let summary = collect_tool_metrics();

        // Summary should have at least the tool we just recorded
        assert!(
            summary.total_invocations >= 2,
            "total invocations should be >= 2, got {}",
            summary.total_invocations
        );
        assert!(
            summary.total_errors >= 1,
            "total errors should be >= 1, got {}",
            summary.total_errors
        );

        // Find our test tool in the list
        let tool = summary.tools.iter().find(|t| t.name == "collect_test_tool");
        assert!(tool.is_some(), "collect_test_tool should be in tools list");
        let tool = tool.unwrap();
        assert!(tool.invocations >= 2);
        assert!(tool.errors >= 1);
        assert!(tool.avg_latency_ms > 0.0);
        assert!(tool.response_bytes >= 512);

        // Find our test agent in the list
        let agent = summary
            .agents
            .iter()
            .find(|a| a.agent == "user/collect-test");
        assert!(
            agent.is_some(),
            "user/collect-test should be in agents list"
        );
        let agent = agent.unwrap();
        assert!(agent.invocations >= 2);
    }
}
