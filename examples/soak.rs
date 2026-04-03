//! Standalone soak test binary for Nellie Production.
//!
//! Runs a long-duration stability test against the full HTTP stack.
//! Designed to run under `nohup`, `tmux`, or `screen` for multi-hour/multi-day runs.
//!
//! # Usage
//!
//! ```bash
//! # Quick validation (2 minutes)
//! cargo run --example soak --release -- --duration 120
//!
//! # 1-hour test
//! cargo run --example soak --release -- --duration 3600
//!
//! # 72-hour production soak (in tmux/screen)
//! cargo run --example soak --release -- --duration 259200 --chunks 50000
//!
//! # With file-backed database
//! cargo run --example soak --release -- --duration 3600 --data-dir /tmp/nellie-soak
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use clap::Parser;
use rand::seq::SliceRandom;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use nellie::server::{
    create_api_router, create_mcp_router, create_rest_router, create_sse_router, create_ui_router,
    App, McpState, ServerConfig,
};
use nellie::storage::{init_storage, insert_chunk, ChunkRecord, Database};

/// Nellie soak test — long-duration stability and performance validation.
#[derive(Parser)]
#[command(name = "nellie-soak", version, about)]
struct Args {
    /// Test duration in seconds.
    #[arg(long, default_value_t = 3600)]
    duration: u64,

    /// Number of chunks to seed before the workload begins.
    #[arg(long, default_value_t = 10000)]
    chunks: usize,

    /// Host to bind the test server on.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to bind the test server on (0 = random).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Directory for a file-backed SQLite database.
    /// If omitted, an in-memory database is used.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

/// Start the Nellie server in-process.
///
/// Returns (base_url, database, server join handle).
async fn start_server(args: &Args) -> (String, Database, tokio::task::JoinHandle<()>) {
    let db = if let Some(ref dir) = args.data_dir {
        std::fs::create_dir_all(dir).expect("failed to create data-dir");
        let db_path = dir.join("nellie-soak.db");
        eprintln!("[SOAK] Using file-backed database: {}", db_path.display());
        Database::open(&db_path).expect("failed to open database")
    } else {
        eprintln!("[SOAK] Using in-memory database");
        Database::open_in_memory().expect("failed to open in-memory database")
    };

    init_storage(&db).expect("failed to initialize storage");

    let config = ServerConfig {
        host: args.host.clone(),
        port: args.port,
        enable_embeddings: false,
        ..Default::default()
    };

    let _app = App::new(config, db.clone())
        .await
        .expect("failed to create app");

    let bind_addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .expect("failed to bind listener");
    let addr = listener.local_addr().expect("failed to get local addr");
    let addr_str = format!("http://{addr}");

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

/// Seed the database with test chunks and lessons.
fn seed_database(db: &Database, num_chunks: usize) {
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
}

/// Compute p50/p95/p99/max from a slice of latencies.
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

/// Get current RSS in MB (Linux-specific, reads /proc/self/statm).
fn get_rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|content| {
            content
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()
                .map(|pages| (pages * 4) / 1024)
        })
        .unwrap_or(0)
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

/// Run PRAGMA integrity_check on the database.
fn check_database_integrity(db: &Database) -> Result<bool, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare("PRAGMA integrity_check").map_err(|e| {
            nellie::Error::internal(format!("failed to prepare integrity check: {e}"))
        })?;
        let result: String = stmt.query_row([], |row| row.get(0)).map_err(|e| {
            nellie::Error::internal(format!("failed to execute integrity check: {e}"))
        })?;
        Ok::<bool, nellie::Error>(result.to_lowercase() == "ok")
    })
    .map_err(|e| format!("database integrity check error: {e}"))
}

/// Build the JSON body for a random operation.
fn build_request_body(op: &str, op_id: u64) -> serde_json::Value {
    match op {
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
                "title": format!("Lesson {op_id}"),
                "content": format!("Content for lesson {op_id}"),
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
        other => panic!("unknown operation: {other}"),
    }
}

/// Run the soak test, returning true on pass, false on fail.
async fn run_soak(args: &Args) -> bool {
    eprintln!(
        "[SOAK] Starting soak test: duration={}s, chunks={}, bind={}:{}",
        args.duration, args.chunks, args.host, args.port,
    );

    let (base_url, db, _server_task) = start_server(args).await;
    eprintln!("[SOAK] Server listening on {base_url}");

    // Seed
    eprintln!("[SOAK] Seeding {} chunks...", args.chunks);
    seed_database(&db, args.chunks);
    eprintln!("[SOAK] Seeding complete");

    let client = reqwest::Client::new();
    let mut rng = rand::thread_rng();

    let mut all_latencies: Vec<u64> = Vec::new();
    let mut window_latencies: Vec<u64> = Vec::new();
    let mut first_window_stats: Option<(u64, u64)> = None;

    // 70% reads, 30% writes (roughly)
    let operations = [
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
    let mut window_start = Instant::now();
    let mut ops_count: u64 = 0;
    let mut error_count: u64 = 0;
    let mut max_single_latency: u64 = 0;

    eprintln!(
        "[SOAK] Starting operation loop for {} seconds...",
        args.duration,
    );

    loop {
        if test_start.elapsed().as_secs() >= args.duration {
            break;
        }

        let op = operations.choose(&mut rng).expect("empty operations");
        let body = build_request_body(op, ops_count);

        let start = Instant::now();
        let response = client
            .post(format!("{base_url}/mcp/invoke"))
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
                        ops_count - 1,
                        resp.status(),
                    );
                }
            }
            Err(e) => {
                error_count += 1;
                eprintln!("[SOAK] Operation {} error: {e}", ops_count - 1);
            }
        }

        // 60-second checkpoint
        if window_start.elapsed().as_secs() >= 60 && !window_latencies.is_empty() {
            let (p50, p95, _p99, _max) = compute_stats(&window_latencies);
            let elapsed_total = test_start.elapsed().as_secs();
            let rss = get_rss_mb();
            let lessons = count_lessons(&db);
            let checkpoints = count_checkpoints(&db);

            eprintln!(
                "[SOAK] elapsed={}s ops={} p50={}ms p95={}ms rss={}MB lessons={} checkpoints={}",
                elapsed_total, ops_count, p50, p95, rss, lessons, checkpoints,
            );

            if first_window_stats.is_none() {
                first_window_stats = Some((p50, p95));
            }

            window_latencies.clear();
            window_start = Instant::now();
        }
    }

    // --- Final report ---
    eprintln!();
    eprintln!("[SOAK] === FINAL REPORT ===");
    eprintln!("[SOAK] Operations: {ops_count}  Errors: {error_count}");

    // Database integrity
    eprintln!("[SOAK] Checking database integrity...");
    let integrity_ok = match check_database_integrity(&db) {
        Ok(true) => {
            eprintln!("[SOAK] Database integrity check PASSED");
            true
        }
        Ok(false) => {
            eprintln!("[SOAK] Database integrity check FAILED");
            false
        }
        Err(e) => {
            eprintln!("[SOAK] Database integrity check error: {e}");
            false
        }
    };

    let (p50_all, p95_all, p99_all, max_all) = compute_stats(&all_latencies);
    eprintln!(
        "[SOAK] Latency: p50={}ms p95={}ms p99={}ms max={}ms",
        p50_all, p95_all, p99_all, max_all,
    );
    eprintln!("[SOAK] RSS: {}MB", get_rss_mb());
    eprintln!(
        "[SOAK] Lessons: {}  Checkpoints: {}",
        count_lessons(&db),
        count_checkpoints(&db)
    );

    // --- Assertions ---
    let mut pass = true;

    if error_count > 0 {
        eprintln!("[SOAK] FAIL: {error_count} errors (expected 0)");
        pass = false;
    }

    if max_single_latency >= 5000 {
        eprintln!("[SOAK] FAIL: max single latency {max_single_latency}ms exceeds 5s limit",);
        pass = false;
    }

    if p95_all >= 5000 {
        eprintln!("[SOAK] FAIL: overall p95 {p95_all}ms exceeds 5s limit");
        pass = false;
    }

    if let Some((_first_p50, first_p95)) = first_window_stats {
        let factor = (p95_all as f64) / (first_p95 as f64);
        eprintln!(
            "[SOAK] p95 drift: first_window={}ms final={}ms factor={:.2}x",
            first_p95, p95_all, factor,
        );
        if factor >= 2.0 {
            eprintln!("[SOAK] FAIL: p95 drift {factor:.2}x exceeds 2x limit",);
            pass = false;
        }
    }

    if !integrity_ok {
        pass = false;
    }

    if pass {
        eprintln!("[SOAK] PASS");
    } else {
        eprintln!("[SOAK] FAIL");
    }

    pass
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    if run_soak(&args).await {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
