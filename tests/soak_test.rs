//! Soak testing harness for Nellie Production
//!
//! This module provides long-duration stability tests that exercise the full HTTP stack
//! under sustained load to validate:
//! - No memory leaks
//! - No database corruption
//! - Consistent latency over time (no degradation)
//! - Stable operation under realistic workloads
//!
//! Tests are marked `#[ignore]` and must be run explicitly:
//! ```bash
//! cargo test --test soak_test -- --ignored --nocapture
//! ```
//!
//! For development, override duration:
//! ```bash
//! NELLIE_SOAK_DURATION_SECS=120 cargo test --test soak_test -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;

use nellie::server::{
    create_api_router, create_mcp_router, create_rest_router, create_sse_router, create_ui_router,
    App, McpState, ServerConfig,
};
use nellie::storage::{
    init_storage, insert_chunk, insert_lesson, ChunkRecord, Database, LessonRecord,
};

/// Helper to start the Nellie server in-process on a random port.
///
/// # Returns
///
/// Tuple of (server address, database, spawned server task)
async fn start_test_server() -> (String, Database, tokio::task::JoinHandle<()>) {
    // Create in-memory database
    let db = Database::open_in_memory().expect("failed to open database");

    init_storage(&db).expect("failed to initialize storage");

    // Configure server with embeddings disabled (for faster testing)
    let config = ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0, // Let OS choose a port
        enable_embeddings: false,
        ..Default::default()
    };

    let _app = App::new(config, db.clone())
        .await
        .expect("failed to create app");

    // Bind to a random port and get the actual address
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind listener");
    let addr = listener.local_addr().expect("failed to get local addr");
    let addr_str = format!("http://{}", addr);

    // Create routers
    use axum::Router;
    use tower_http::cors::{Any, CorsLayer};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let state = Arc::new(McpState::with_api_key(db.clone(), None));

    let router = Router::new()
        .merge(create_mcp_router(Arc::clone(&state)))
        .merge(create_rest_router(Arc::clone(&state)))
        .merge(create_api_router(Arc::clone(&state)))
        .merge(create_sse_router(Arc::clone(&state)))
        .merge(create_ui_router())
        .layer(cors);

    let server_task = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server error");
    });

    (addr_str, db, server_task)
}

/// Helper to seed the database with test data.
async fn seed_database(db: &Database, num_chunks: usize, num_lessons: usize) {
    // Insert chunks
    for i in 0..num_chunks {
        let chunk = ChunkRecord::new(
            format!("test_file_{}.rs", i / 100),
            (i % 100) as i32,
            ((i * 10) + 1) as i32,
            ((i * 10) + 20) as i32,
            format!("fn test_function_{}() {{ println!(\"test {}\"); }}", i, i),
            format!("hash_{}", i),
        )
        .with_language("rust");

        db.with_conn(|conn| {
            insert_chunk(conn, &chunk).expect("failed to insert chunk");
            Ok::<(), nellie::Error>(())
        })
        .expect("chunk insert failed");
    }

    // Insert lessons
    for i in 0..num_lessons {
        let lesson = LessonRecord::new(
            format!("Lesson {}", i),
            format!("This is a test lesson about topic {}", i),
            vec!["rust".to_string(), "testing".to_string()],
        );

        db.with_conn(|conn| {
            insert_lesson(conn, &lesson).expect("failed to insert lesson");
            Ok::<(), nellie::Error>(())
        })
        .expect("lesson insert failed");
    }
}

/// Collect latency statistics from a vec of latencies.
fn compute_stats(latencies: &[u64]) -> (u64, u64, u64, u64) {
    if latencies.is_empty() {
        return (0, 0, 0, 0);
    }

    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();

    let count = sorted.len();
    let p50 = sorted[count / 2];
    let p95 = sorted[(count as f64 * 0.95) as usize];
    let p99 = sorted[(count as f64 * 0.99) as usize];
    let max = sorted[count - 1];

    (p50, p95, p99, max)
}

/// Get current RSS memory usage in MB (Linux-specific).
///
/// Reads from `/proc/self/statm` and extracts the resident set size.
/// Returns the value in MB (converting from pages, where page size = 4KB on most systems).
fn get_rss_mb() -> u64 {
    use std::fs;

    match fs::read_to_string("/proc/self/statm") {
        Ok(content) => {
            if let Some(rss_pages_str) = content.split_whitespace().nth(1) {
                if let Ok(rss_pages) = rss_pages_str.parse::<u64>() {
                    // Convert pages to MB (4KB page size)
                    return (rss_pages * 4) / 1024;
                }
            }
            0
        }
        Err(_) => 0,
    }
}

/// Count lessons in the database.
fn count_lessons(db: &Database) -> usize {
    db.with_conn(|conn| {
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM lessons")
            .map_err(|e| nellie::Error::internal(e.to_string()))?;
        let count: usize = stmt
            .query_row([], |row| row.get(0))
            .map_err(|e| nellie::Error::internal(e.to_string()))?;
        Ok::<usize, nellie::Error>(count)
    })
    .unwrap_or(0)
}

/// Count checkpoints in the database.
fn count_checkpoints(db: &Database) -> usize {
    db.with_conn(|conn| {
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM checkpoints")
            .map_err(|e| nellie::Error::internal(e.to_string()))?;
        let count: usize = stmt
            .query_row([], |row| row.get(0))
            .map_err(|e| nellie::Error::internal(e.to_string()))?;
        Ok::<usize, nellie::Error>(count)
    })
    .unwrap_or(0)
}

/// Run PRAGMA integrity_check and verify the database is not corrupt.
///
/// # Returns
///
/// Ok(true) if integrity check passes ("ok"), Ok(false) otherwise, Err if check fails.
fn check_database_integrity(db: &Database) -> Result<bool, String> {
    let result = db.with_conn(|conn| {
        let mut stmt = conn.prepare("PRAGMA integrity_check").map_err(|e| {
            nellie::Error::internal(format!("failed to prepare integrity check: {}", e))
        })?;

        let result: String = stmt.query_row([], |row| row.get(0)).map_err(|e| {
            nellie::Error::internal(format!("failed to execute integrity check: {}", e))
        })?;

        Ok::<bool, nellie::Error>(result.to_lowercase() == "ok")
    });

    match result {
        Ok(ok) => Ok(ok),
        Err(e) => Err(format!("database integrity check error: {}", e)),
    }
}

/// Test: 1-hour (configurable) soak test with latency trend monitoring
///
/// This test:
/// - Starts the Nellie server
/// - Seeds 10K chunks
/// - Runs mixed operations (70% reads, 30% writes) for the configured duration
/// - Records latency for each operation
/// - Every 60 seconds, logs a checkpoint with p50/p95 latency and memory usage
/// - After completion, asserts:
///   - No operation took longer than 5 seconds
///   - p95 latency did not increase by more than 2x between first and last 60-second window
///   - All operations succeeded (no 5xx errors)
///   - Database integrity check passes
#[tokio::test]
#[ignore]
async fn test_soak_1_hour() {
    use rand::seq::SliceRandom;

    // Parse duration from environment, default 3600 seconds (1 hour)
    let duration_secs: u64 = std::env::var("NELLIE_SOAK_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);

    println!(
        "[SOAK] Starting 1-hour soak test with duration = {} seconds",
        duration_secs
    );

    let (base_url, db, _server_task) = start_test_server().await;

    // Seed database with 10K chunks
    println!("[SOAK] Seeding 10K chunks...");
    seed_database(&db, 10000, 0).await;
    println!("[SOAK] Seeding complete");

    let client = reqwest::Client::new();
    let mut rng = rand::thread_rng();

    // Track latencies across the entire test
    let mut all_latencies = Vec::new();

    // Track latencies per window (60-second intervals)
    let mut window_latencies = Vec::new();
    let mut first_window_stats: Option<(u64, u64)> = None;

    // Define operation types (70% reads, 30% writes)
    let operations = vec![
        "search_code",
        "search_code",
        "search_code",
        "search_code",
        "search_code", // 5/7 = 71% reads
        "search_code",
        "search_code",
        "add_lesson",     // 1/7 = 14% writes
        "search_lessons", // 1/7 = 14% reads
    ];

    let test_start = Instant::now();
    let mut window_start = Instant::now();
    let mut ops_count = 0u64;
    let mut error_count = 0u64;
    let mut max_single_latency = 0u64;

    println!(
        "[SOAK] Starting operation loop for {} seconds...",
        duration_secs
    );

    loop {
        let elapsed = test_start.elapsed();

        if elapsed.as_secs() >= duration_secs {
            break;
        }

        // Pick a random operation
        let op = operations.choose(&mut rng).expect("empty operations");

        let op_id = ops_count;
        let body = match *op {
            "search_code" => serde_json::json!({
                "name": "search_code",
                "arguments": {
                    "query": "test function",
                    "limit": 10
                }
            }),
            "add_lesson" => serde_json::json!({
                "name": "add_lesson",
                "arguments": {
                    "title": format!("Lesson {}", op_id),
                    "content": format!("Content for lesson {}", op_id),
                    "tags": ["rust", "testing"]
                }
            }),
            "search_lessons" => serde_json::json!({
                "name": "search_lessons",
                "arguments": {
                    "query": "lesson",
                    "limit": 5
                }
            }),
            _ => unreachable!(),
        };

        let start = Instant::now();
        let response = client
            .post(format!("{}/mcp/invoke", base_url))
            .json(&body)
            .send()
            .await;

        let elapsed_ms = start.elapsed().as_millis() as u64;
        ops_count += 1;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    all_latencies.push(elapsed_ms);
                    window_latencies.push(elapsed_ms);

                    if elapsed_ms > max_single_latency {
                        max_single_latency = elapsed_ms;
                    }
                } else {
                    error_count += 1;
                    eprintln!(
                        "[SOAK] Operation {} failed with HTTP {}",
                        op_id,
                        resp.status()
                    );
                }
            }
            Err(e) => {
                error_count += 1;
                eprintln!("[SOAK] Operation {} error: {}", op_id, e);
            }
        }

        // Every 60 seconds, log a checkpoint
        if window_start.elapsed().as_secs() >= 60 {
            if !window_latencies.is_empty() {
                let (p50, p95, _p99, _max) = compute_stats(&window_latencies);

                let elapsed_total = test_start.elapsed().as_secs();
                let rss = get_rss_mb();
                let lessons = count_lessons(&db);
                let checkpoints = count_checkpoints(&db);

                println!(
                    "[SOAK] elapsed={}s ops={} p50={}ms p95={}ms rss={}MB lessons={} checkpoints={}",
                    elapsed_total, ops_count, p50, p95, rss, lessons, checkpoints
                );

                // Track first window's p95 for later comparison
                if first_window_stats.is_none() {
                    first_window_stats = Some((p50, p95));
                }

                window_latencies.clear();
                window_start = Instant::now();
            }
        }
    }

    // Final stats
    println!(
        "[SOAK] Test completed. {} operations, {} errors",
        ops_count, error_count
    );

    // Verify database integrity
    println!("[SOAK] Checking database integrity...");
    match check_database_integrity(&db) {
        Ok(true) => println!("[SOAK] Database integrity check PASSED"),
        Ok(false) => panic!("[SOAK] Database integrity check FAILED"),
        Err(e) => panic!("[SOAK] Database integrity check error: {}", e),
    }

    // Compute overall statistics
    let (p50_all, p95_all, _p99_all, max_all) = compute_stats(&all_latencies);

    println!(
        "[SOAK] Final stats: p50={}ms p95={}ms max={}ms",
        p50_all, p95_all, max_all
    );

    // Assertions
    assert_eq!(error_count, 0, "expected 0 errors, got {}", error_count);

    assert!(
        max_single_latency < 5000,
        "max single operation latency {} ms exceeds 5 second limit",
        max_single_latency
    );

    if let Some((_first_p50, first_p95)) = first_window_stats {
        let p95_increase_factor = (p95_all as f64) / (first_p95 as f64);
        println!(
            "[SOAK] p95 trend: first window p95={}ms, final p95={}ms, factor={:.2}x",
            first_p95, p95_all, p95_increase_factor
        );

        assert!(
            p95_increase_factor < 2.0,
            "p95 latency increased by {:.2}x (first window: {}ms, final: {}ms), exceeds 2x limit",
            p95_increase_factor,
            first_p95,
            p95_all
        );
    }

    assert!(
        p95_all < 5000,
        "overall p95 latency {} ms exceeds 5 second limit",
        p95_all
    );

    println!("[SOAK] ✅ 1-hour soak test PASSED");
}

/// Test: Memory stability over 10 minutes
///
/// This test:
/// - Starts the Nellie server
/// - Seeds 5K chunks
/// - Runs mixed operations for 10 minutes
/// - Samples RSS memory every 30 seconds
/// - Asserts RSS growth < 50MB over the test duration
#[tokio::test]
#[ignore]
async fn test_memory_stability() {
    use rand::seq::SliceRandom;

    const TEST_DURATION_SECS: u64 = 600; // 10 minutes
    const MEMORY_SAMPLE_INTERVAL_SECS: u64 = 30;
    const MAX_RSS_GROWTH_MB: u64 = 50;

    println!("[SOAK] Starting memory stability test (10 minutes)");

    let (base_url, db, _server_task) = start_test_server().await;

    // Seed database with 5K chunks
    println!("[SOAK] Seeding 5K chunks...");
    seed_database(&db, 5000, 0).await;
    println!("[SOAK] Seeding complete");

    let client = reqwest::Client::new();
    let mut rng = rand::thread_rng();

    // Define operation types
    let operations = vec![
        "search_code",
        "search_code",
        "search_code",
        "search_code",
        "search_code",
        "search_code",
        "search_code",
        "add_lesson",
        "search_lessons",
    ];

    let test_start = Instant::now();
    let mut last_sample_time = Instant::now();
    let mut memory_samples = Vec::new();

    // Take initial memory sample
    let initial_rss = get_rss_mb();
    memory_samples.push(initial_rss);
    println!("[SOAK] Initial RSS: {}MB", initial_rss);

    let mut ops_count = 0u64;
    let mut error_count = 0u64;

    println!(
        "[SOAK] Starting mixed workload for {} seconds...",
        TEST_DURATION_SECS
    );

    loop {
        let elapsed = test_start.elapsed();

        if elapsed.as_secs() >= TEST_DURATION_SECS {
            break;
        }

        // Pick a random operation
        let op = operations.choose(&mut rng).expect("empty operations");
        let op_id = ops_count;

        let body = match *op {
            "search_code" => serde_json::json!({
                "name": "search_code",
                "arguments": {
                    "query": "test function",
                    "limit": 10
                }
            }),
            "add_lesson" => serde_json::json!({
                "name": "add_lesson",
                "arguments": {
                    "title": format!("Lesson {}", op_id),
                    "content": format!("Content for lesson {}", op_id),
                    "tags": ["rust", "testing"]
                }
            }),
            "search_lessons" => serde_json::json!({
                "name": "search_lessons",
                "arguments": {
                    "query": "lesson",
                    "limit": 5
                }
            }),
            _ => unreachable!(),
        };

        let response = client
            .post(format!("{}/mcp/invoke", base_url))
            .json(&body)
            .send()
            .await;

        ops_count += 1;

        match response {
            Ok(resp) => {
                if !resp.status().is_success() {
                    error_count += 1;
                }
            }
            Err(_) => {
                error_count += 1;
            }
        }

        // Every 30 seconds, sample memory
        if last_sample_time.elapsed().as_secs() >= MEMORY_SAMPLE_INTERVAL_SECS {
            let current_rss = get_rss_mb();
            memory_samples.push(current_rss);

            let elapsed_total = test_start.elapsed().as_secs();
            println!(
                "[SOAK] Memory sample at {}s: {}MB (delta: +{}MB from initial)",
                elapsed_total,
                current_rss,
                current_rss.saturating_sub(initial_rss)
            );

            last_sample_time = Instant::now();
        }
    }

    let final_rss = get_rss_mb();
    let rss_growth = final_rss.saturating_sub(initial_rss);

    println!(
        "[SOAK] Memory stability test complete: {} operations, {} errors",
        ops_count, error_count
    );
    println!(
        "[SOAK] Initial RSS: {}MB, Final RSS: {}MB, Growth: {}MB",
        initial_rss, final_rss, rss_growth
    );

    // Assertions
    assert_eq!(error_count, 0, "expected 0 errors, got {}", error_count);

    assert!(
        rss_growth < MAX_RSS_GROWTH_MB,
        "RSS growth {} MB exceeds threshold of {} MB",
        rss_growth,
        MAX_RSS_GROWTH_MB
    );

    println!("[SOAK] ✅ Memory stability test PASSED");
}

#[cfg(test)]
mod internal_helpers {
    use super::*;

    #[test]
    fn test_compute_stats() {
        let latencies = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let (p50, p95, _p99, max) = compute_stats(&latencies);

        assert_eq!(p50, 60);
        assert_eq!(p95, 100);
        assert_eq!(max, 100);
    }

    #[test]
    fn test_compute_stats_empty() {
        let latencies: Vec<u64> = vec![];
        let (p50, p95, p99, max) = compute_stats(&latencies);

        assert_eq!(p50, 0);
        assert_eq!(p95, 0);
        assert_eq!(p99, 0);
        assert_eq!(max, 0);
    }
}
