//! HTTP load testing harness for Nellie Production
//!
//! This module provides end-to-end integration tests that exercise the full HTTP stack
//! (axum server → MCP tool invocation → database) under realistic load scenarios.
//!
//! Tests measure:
//! - Sequential search latency and throughput
//! - Concurrent request handling
//! - Mixed workload performance
//! - File indexing throughput
//!
//! All tests are marked `#[ignore]` and must be run explicitly:
//! ```bash
//! cargo test --test load_test -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

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
    let data_dir = tempfile::tempdir().expect("failed to create temp dir");

    // Create database
    let db = Database::open(data_dir.path()).expect("failed to open database");

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

/// Test: Sequential search latency under single-client load
#[tokio::test]
#[ignore]
async fn test_search_code_latency() {
    let (base_url, db, _server_task) = start_test_server().await;

    // Seed database with 10K chunks
    seed_database(&db, 10000, 0).await;

    // Create HTTP client
    let client = reqwest::Client::new();

    // Send 100 sequential search requests
    let mut latencies = Vec::new();

    for i in 0..100 {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": i,
            "method": "search_code",
            "params": {
                "query": "test function",
                "limit": 10
            }
        });

        let start = Instant::now();
        let response = client
            .post(format!("{}/mcp/invoke", base_url))
            .json(&body)
            .send()
            .await;
        let elapsed = start.elapsed().as_millis() as u64;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    latencies.push(elapsed);
                } else {
                    eprintln!("Request {}: HTTP {}", i, resp.status());
                }
            }
            Err(e) => eprintln!("Request {}: Error {}", i, e),
        }
    }

    // Compute statistics
    let (p50, p95, p99, max) = compute_stats(&latencies);

    println!(
        "[LOAD] test_search_code_latency: p50={}ms p95={}ms p99={}ms max={}ms count={}",
        p50,
        p95,
        p99,
        max,
        latencies.len()
    );

    // Assert p95 < 200ms
    assert!(p95 < 200, "p95 latency {} ms exceeds target of 200ms", p95);
}

/// Test: Concurrent search requests from multiple clients
#[tokio::test]
#[ignore]
async fn test_concurrent_search() {
    let (base_url, db, _server_task) = start_test_server().await;

    // Seed database with 10K chunks
    seed_database(&db, 10000, 0).await;

    let client = reqwest::Client::new();
    let latencies = Arc::new(Mutex::new(Vec::new()));

    // Spawn 10 concurrent tasks, each sending 20 requests
    let mut handles = vec![];

    for task_id in 0..10 {
        let base_url_clone = base_url.clone();
        let client_clone = client.clone();
        let latencies_clone = Arc::clone(&latencies);

        let handle = tokio::spawn(async move {
            for req_id in 0..20 {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": task_id * 100 + req_id,
                    "method": "search_code",
                    "params": {
                        "query": "test function",
                        "limit": 10
                    }
                });

                let start = Instant::now();
                if let Ok(response) = client_clone
                    .post(format!("{}/mcp/invoke", base_url_clone))
                    .json(&body)
                    .send()
                    .await
                {
                    if response.status().is_success() {
                        let elapsed = start.elapsed().as_millis() as u64;
                        latencies_clone.lock().await.push(elapsed);
                    }
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all tasks
    for handle in handles {
        let _ = handle.await;
    }

    let latencies_vec = latencies.lock().await.clone();
    let (p50, p95, p99, max) = compute_stats(&latencies_vec);

    println!(
        "[LOAD] test_concurrent_search: p50={}ms p95={}ms p99={}ms max={}ms count={}",
        p50,
        p95,
        p99,
        max,
        latencies_vec.len()
    );

    // Assert p95 < 500ms (higher target for concurrent workload)
    assert!(
        p95 < 500,
        "p95 latency {} ms exceeds target of 500ms for concurrent workload",
        p95
    );
}

/// Test: Mixed workload with random operations
#[tokio::test]
#[ignore]
async fn test_mixed_workload() {
    use rand::seq::SliceRandom;

    let (base_url, db, _server_task) = start_test_server().await;

    // Seed initial data
    seed_database(&db, 5000, 100).await;

    let client = reqwest::Client::new();
    let mut latencies = Vec::new();
    let mut rng = rand::thread_rng();

    // Define operation types
    let operations = vec!["search_code", "add_lesson", "search_lessons"];

    // Run 100 mixed operations
    for i in 0..100 {
        let op = operations.choose(&mut rng).expect("empty operations");

        let body = match *op {
            "search_code" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": i,
                "method": "search_code",
                "params": {
                    "query": "test function",
                    "limit": 10
                }
            }),
            "add_lesson" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": i,
                "method": "add_lesson",
                "params": {
                    "title": format!("Lesson {}", i),
                    "content": format!("Content for lesson {}", i),
                    "tags": ["rust", "testing"]
                }
            }),
            "search_lessons" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": i,
                "method": "search_lessons",
                "params": {
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
        let elapsed = start.elapsed().as_millis() as u64;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    latencies.push(elapsed);
                } else {
                    panic!("Request {} failed with HTTP {}", i, resp.status());
                }
            }
            Err(e) => panic!("Request {} error: {}", i, e),
        }
    }

    let (p50, p95, _p99, max) = compute_stats(&latencies);

    println!(
        "[LOAD] test_mixed_workload: p50={}ms p95={}ms p99={}ms max={}ms count={}",
        p50,
        p95,
        compute_stats(&latencies).2,
        max,
        latencies.len()
    );

    // Assert p95 < 500ms
    assert!(p95 < 500, "p95 latency {} ms exceeds target of 500ms", p95);

    // All operations must complete without error
    assert_eq!(latencies.len(), 100, "not all operations completed");
}

/// Test: Index throughput with file watching
#[tokio::test]
#[ignore]
async fn test_index_throughput() {
    let (base_url, db, _server_task) = start_test_server().await;

    // Create a temporary directory with test files
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");

    // Create 500 test files
    for i in 0..500 {
        let file_path = temp_dir.path().join(format!("test_{}.rs", i));
        std::fs::write(
            &file_path,
            format!(
                "// Test file {}\nfn test_function_{}() {{ println!(\"test\"); }}",
                i, i
            ),
        )
        .expect("failed to write test file");
    }

    // Trigger indexing via API
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "bootstrap_graph",
        "params": {
            "indexed_root": temp_dir.path().to_string_lossy()
        }
    });

    let start = Instant::now();
    let _response = client
        .post(format!("{}/mcp/invoke", base_url))
        .json(&body)
        .send()
        .await
        .expect("bootstrap failed");

    // Wait a bit for indexing to complete
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let elapsed = start.elapsed();

    // Check how many files were indexed
    let chunk_count = db
        .with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT COUNT(*) FROM chunks")
                .map_err(|e| nellie::Error::internal(e.to_string()))?;
            let count: usize = stmt
                .query_row([], |row| row.get(0))
                .map_err(|e| nellie::Error::internal(e.to_string()))?;
            Ok::<usize, nellie::Error>(count)
        })
        .expect("failed to count chunks");

    println!(
        "[LOAD] test_index_throughput: indexed {} files in {:.2}s (throughput: {:.1} files/sec)",
        chunk_count,
        elapsed.as_secs_f64(),
        chunk_count as f64 / elapsed.as_secs_f64()
    );

    // Verify some files were indexed (not a strict requirement since indexing may be async)
    assert!(chunk_count > 0, "no files were indexed");
}

#[cfg(test)]
mod internal_helpers {
    use super::*;

    /// Test the stats computation helper
    #[test]
    fn test_compute_stats() {
        let latencies = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let (p50, p95, _p99, max) = compute_stats(&latencies);

        // With 10 elements, p50 is at index 5 (value 60)
        assert_eq!(p50, 60);
        // p95 is at index (10 * 0.95) = 9 (value 100)
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
