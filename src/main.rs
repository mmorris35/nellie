//! Nellie Production - Semantic code memory system
//!
//! Entry point for the Nellie server with CLI subcommands.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]

use clap::{Parser, Subcommand};
use nellie::server::{init_metrics, init_tracing, App, ServerConfig};
use nellie::storage::{init_storage, Database};
use nellie::watcher::{FileFilter, FileWatcher, IndexRequest, Indexer, WatcherConfig};
use nellie::{Config, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Returns the default data directory: `~/.local/share/nellie`
/// Falls back to `./data` if the home directory cannot be determined.
fn default_data_dir() -> PathBuf {
    dirs::data_local_dir().map_or_else(|| PathBuf::from("./data"), |d| d.join("nellie"))
}

/// Nellie Production - Semantic code memory system for enterprise teams
///
/// A production-grade semantic code search engine with AI-powered indexing,
/// lessons management, and agent checkpoints.
#[derive(Parser, Debug)]
#[command(name = "nellie")]
#[command(version)]
#[command(long_about = None)]
#[command(about = "Semantic code memory system for enterprise engineering teams")]
struct Cli {
    /// Data directory for `SQLite` database
    #[arg(
        short,
        long,
        env = "NELLIE_DATA_DIR",
        default_value_os_t = default_data_dir(),
        global = true
    )]
    data_dir: PathBuf,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, env = "NELLIE_LOG_LEVEL", default_value = "info", global = true)]
    log_level: String,

    /// Enable JSON logging output
    #[arg(long, env = "NELLIE_LOG_JSON", global = true)]
    log_json: bool,

    /// API key for authentication (required for production use)
    #[arg(long, env = "NELLIE_API_KEY", global = true)]
    api_key: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the Nellie server
    ///
    /// Starts the MCP and REST API server for semantic code search,
    /// lessons management, and agent checkpoints. Optionally watches
    /// specified directories for automatic indexing.
    Serve {
        /// Host address to bind to
        #[arg(long, env = "NELLIE_HOST", default_value = "127.0.0.1")]
        host: String,

        /// Port to listen on
        #[arg(short, long, env = "NELLIE_PORT", default_value = "8765")]
        port: u16,

        /// Directories to watch for code changes (comma-separated)
        #[arg(short, long, env = "NELLIE_WATCH_DIRS", value_delimiter = ',')]
        watch: Vec<PathBuf>,

        /// Number of embedding worker threads
        #[arg(long, env = "NELLIE_EMBEDDING_THREADS", default_value = "4")]
        embedding_threads: usize,

        /// Disable embedding service (semantic search will not work)
        #[arg(long, env = "NELLIE_DISABLE_EMBEDDINGS")]
        disable_embeddings: bool,

        /// Enable Nellie-V graph memory layer
        #[arg(long, env = "NELLIE_ENABLE_GRAPH")]
        enable_graph: bool,

        /// Enable Tree-sitter structural code analysis
        #[arg(long, env = "NELLIE_ENABLE_STRUCTURAL")]
        enable_structural: bool,

        /// Enable Deep Hooks daemon (auto-ingest transcripts, periodic sync)
        #[arg(long, env = "NELLIE_ENABLE_DEEP_HOOKS")]
        enable_deep_hooks: bool,

        /// Periodic sync interval in minutes (default: 30)
        #[arg(long, env = "NELLIE_SYNC_INTERVAL", default_value = "30")]
        sync_interval: u64,

        /// Skip the initial filesystem walk on startup (use DB-first reconciliation only).
        /// Useful for resume-after-crash scenarios where a walk would be wasteful.
        #[arg(long, default_value_t = false)]
        skip_initial_walk: bool,
    },

    /// Manually index a directory
    ///
    /// Triggers immediate indexing of one or more directories.
    /// Useful for forcing re-indexing without waiting for file watcher.
    Index {
        /// Path(s) to index (comma-separated)
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Nellie server URL. If set, routes through the running server via HTTP
        /// instead of opening the DB directly. Avoids dual-writer corruption.
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        server: String,

        /// Force local indexing even if a server is reachable
        #[arg(long, default_value_t = false)]
        local: bool,
    },

    /// Search for code semantically
    ///
    /// Performs a semantic search across indexed code.
    /// Requires the server to be running in another terminal.
    Search {
        /// Search query (natural language or code keywords)
        #[arg(value_name = "QUERY")]
        query: String,

        /// Maximum number of results
        #[arg(short, long, default_value = "10")]
        limit: usize,

        /// Minimum similarity score (0.0-1.0)
        #[arg(long, default_value = "0.5")]
        threshold: f32,

        /// Server URL
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        server: String,
    },

    /// Show server status and statistics
    ///
    /// Displays current server status, configuration, and indexed statistics.
    /// Requires the server to be running.
    Status {
        /// Server URL
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        server: String,

        /// Output format (text or json)
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Sync Nellie knowledge to Claude Code memory files
    ///
    /// Reads lessons and checkpoints from Nellie's database and writes
    /// them as Claude Code memory files with YAML frontmatter. Updates
    /// MEMORY.md index, cleans up stale entries, and enforces the
    /// 200-line budget.
    ///
    /// When --rules is passed, also generates glob-conditioned rule
    /// files in ~/.claude/rules/ from critical/warning severity lessons
    /// that have tags.
    Sync {
        /// Target project directory (default: current working directory)
        #[arg(long)]
        project: Option<PathBuf>,

        /// Show what would be written without actually writing files
        #[arg(long)]
        dry_run: bool,

        /// Maximum number of lessons to sync (default: 50)
        #[arg(long, default_value = "50")]
        max_lessons: usize,

        /// Maximum number of checkpoints to sync (default: 3)
        #[arg(long, default_value = "3")]
        max_checkpoints: usize,

        /// Maximum line budget for MEMORY.md (default: 180)
        ///
        /// Claude Code truncates MEMORY.md at 200 lines, so this
        /// default reserves 20 lines for non-Nellie entries.
        /// After deduplication and scoring, only memories that fit
        /// within this budget are written.
        #[arg(long, default_value = "180")]
        budget: usize,

        /// Also sync rules to ~/.claude/rules/
        ///
        /// Generates glob-conditioned rule files from critical and
        /// warning severity lessons with tags. Cleans up stale rules.
        #[arg(long)]
        rules: bool,

        /// Remote Nellie server URL for cross-machine operation
        ///
        /// When set, queries lessons and checkpoints from the remote
        /// server instead of the local database. Memory files are
        /// still written locally.
        /// Example: --server http://localhost:8765
        #[arg(long, env = "NELLIE_SERVER")]
        server: Option<String>,
    },

    /// Ingest Claude Code session transcripts for passive learning
    ///
    /// Parses JSONL session transcripts, extracts learnable patterns
    /// (corrections, failures, repeated tools, explicit saves), deduplicates
    /// against existing lessons, and stores new ones in Nellie's database.
    ///
    /// Supports two modes:
    /// 1. Single file: nellie ingest <TRANSCRIPT_PATH> [--dry-run]
    /// 2. Batch: nellie ingest --project <dir> [--since <timestamp>] [--dry-run]
    Ingest {
        /// Path to a single .jsonl transcript file to ingest
        #[arg(value_name = "TRANSCRIPT_PATH")]
        transcript: Option<PathBuf>,

        /// Project directory to batch-scan for .jsonl transcripts
        ///
        /// Scans ~/.claude/projects/<project>/ for .jsonl files.
        #[arg(long)]
        project: Option<PathBuf>,

        /// Only ingest transcripts modified since this time
        ///
        /// Supports Unix timestamps or human-readable formats like "1h", "2d", "30m", "1w".
        /// Use with --project for incremental ingest. Examples:
        /// nellie ingest --project ~/github/nellie-rs --since 1630000000
        /// nellie ingest --project ~/github/nellie-rs --since 1h
        #[arg(long)]
        since: Option<String>,

        /// Show what would be ingested without actually storing
        #[arg(long)]
        dry_run: bool,

        /// Remote Nellie server URL for cross-machine operation
        ///
        /// When set, POSTs extracted lessons to the remote server
        /// instead of storing in the local database.
        /// Example: --server http://localhost:8765
        #[arg(long, env = "NELLIE_SERVER")]
        server: Option<String>,
    },

    /// Inject Nellie context into Claude Code for the current prompt
    ///
    /// Searches Nellie for lessons and context relevant to the user's prompt,
    /// filters by relevance threshold, and writes them to a temporary rules file
    /// that Claude Code loads before processing the prompt.
    /// This enables automatic knowledge enrichment without explicit memory files.
    ///
    /// Designed to be called via the UserPromptSubmit hook with the user's
    /// prompt text as the query.
    Inject {
        /// Search query (typically the user's prompt text)
        #[arg(long)]
        query: String,

        /// Maximum number of results to inject
        #[arg(long, default_value = "3")]
        limit: usize,

        /// Minimum relevance score (0.0-1.0, higher = more relevant)
        #[arg(long, default_value = "0.4")]
        threshold: f64,

        /// Timeout in milliseconds
        #[arg(long, default_value = "800")]
        timeout: u64,

        /// Nellie server URL for remote operation
        #[arg(long, env = "NELLIE_SERVER")]
        server: Option<String>,

        /// Show what would be injected without writing files
        #[arg(long)]
        dry_run: bool,
    },

    /// Install Nellie hooks in Claude Code settings.json
    ///
    /// Adds SessionStart (sync) and Stop (ingest) hooks to
    /// ~/.claude/settings.json. Preserves existing hooks and creates
    /// a backup (settings.json.bak) before modification.
    #[command(name = "hooks-install")]
    HooksInstall {
        /// Force reinstall (remove existing Nellie hooks first)
        #[arg(long)]
        force: bool,

        /// Nellie server URL for remote operation (baked into hook commands)
        #[arg(long)]
        server: Option<String>,
    },

    /// Remove Nellie hooks from Claude Code settings.json
    ///
    /// Removes only Nellie hooks, preserving all other hooks.
    /// No backup is created for uninstall.
    #[command(name = "hooks-uninstall")]
    HooksUninstall,

    /// Check the status of Nellie hooks
    ///
    /// Shows whether hooks are installed, if the nellie binary is on PATH,
    /// and other system health metrics (last sync/ingest times, memory/rule
    /// file counts, etc.).
    #[command(name = "hooks-status")]
    HooksStatus {
        /// Output in JSON format instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Download ONNX Runtime, embedding model, and tokenizer
    ///
    /// For users who build Nellie from source (`cargo build --release`),
    /// this command downloads the required runtime files. Equivalent to
    /// what `packaging/install-universal.sh` does for the quick-install path.
    /// Files are verified with SHA-256 checksums. Re-running is safe
    /// (existing files are skipped).
    #[command(name = "setup")]
    Setup {
        /// Skip ONNX Runtime download
        #[arg(long)]
        skip_runtime: bool,

        /// Skip embedding model download
        #[arg(long)]
        skip_model: bool,
    },

    /// Import starter lessons into the database
    ///
    /// Reads embedded bootstrap lesson files and imports them into the
    /// Nellie database. Lessons are imported idempotently -- if a lesson
    /// with the same title already exists, it is skipped unless --force
    /// is used.
    Bootstrap {
        /// Re-import all lessons even if they already exist (deletes + re-inserts)
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Check if this is a JSON output command that shouldn't have logs
    let suppress_startup_logs = match &cli.command {
        Some(Commands::HooksStatus { json: true }) => true,
        Some(Commands::Status { format, .. }) if format == "json" => true,
        _ => false,
    };

    // Initialize tracing with configuration
    init_tracing(&cli.log_level, cli.log_json);

    // Only emit startup log for non-JSON commands
    if !suppress_startup_logs {
        tracing::info!(
            "Nellie Production v{} - Semantic code memory system",
            env!("CARGO_PKG_VERSION")
        );
    }

    // Route to appropriate command handler
    match cli.command {
        Some(Commands::Serve {
            host,
            port,
            watch,
            embedding_threads,
            disable_embeddings,
            enable_graph,
            enable_structural,
            enable_deep_hooks,
            sync_interval,
            skip_initial_walk,
        }) => {
            serve_command(ServeCommandArgs {
                data_dir: cli.data_dir,
                host,
                port,
                watch,
                embedding_threads,
                log_level: cli.log_level,
                api_key: cli.api_key,
                disable_embeddings,
                enable_graph,
                enable_structural,
                enable_deep_hooks,
                sync_interval,
                skip_initial_walk,
            })
            .await
        }
        Some(Commands::Index {
            paths,
            server,
            local,
        }) => index_command(paths, server, local, cli.data_dir).await,
        Some(Commands::Search {
            query,
            limit,
            threshold,
            server,
        }) => search_command(query, limit, threshold, server),
        Some(Commands::Status { server, format }) => status_command(server, format),
        Some(Commands::Sync {
            project,
            dry_run,
            max_lessons,
            max_checkpoints,
            budget,
            rules,
            server,
        }) => {
            sync_command(
                cli.data_dir,
                project,
                dry_run,
                max_lessons,
                max_checkpoints,
                budget,
                rules,
                server,
            )
            .await
        }
        Some(Commands::Ingest {
            transcript,
            project,
            since,
            dry_run,
            server,
        }) => ingest_command(cli.data_dir, transcript, project, since, dry_run, server).await,
        Some(Commands::Inject {
            query,
            limit,
            threshold,
            timeout,
            server,
            dry_run,
        }) => {
            inject_command(
                &query,
                limit,
                threshold,
                timeout,
                server.as_deref(),
                dry_run,
            )
            .await
        }
        Some(Commands::HooksInstall { force, server }) => {
            hooks_install_command(force, server.as_deref())
        }
        Some(Commands::HooksUninstall) => hooks_uninstall_command(),
        Some(Commands::HooksStatus { json }) => hooks_status_command(json),
        Some(Commands::Setup {
            skip_runtime,
            skip_model,
        }) => setup_command(&cli.data_dir, skip_runtime, skip_model).await,
        Some(Commands::Bootstrap { force }) => bootstrap_command(&cli.data_dir, force).await,
        None => {
            // Default to serve command for backward compatibility
            tracing::info!("No command specified, starting server (use 'serve' explicitly)");
            serve_command(ServeCommandArgs {
                data_dir: cli.data_dir,
                host: "127.0.0.1".to_string(),
                port: 8765,
                watch: vec![],
                embedding_threads: 4,
                log_level: cli.log_level,
                api_key: cli.api_key,
                disable_embeddings: false,
                enable_graph: false,
                enable_structural: false,
                enable_deep_hooks: false,
                sync_interval: 30,
                skip_initial_walk: false,
            })
            .await
        }
    }
}

/// Command arguments for serve subcommand.
#[allow(clippy::struct_excessive_bools)]
struct ServeCommandArgs {
    data_dir: PathBuf,
    host: String,
    port: u16,
    watch: Vec<PathBuf>,
    embedding_threads: usize,
    log_level: String,
    api_key: Option<String>,
    disable_embeddings: bool,
    enable_graph: bool,
    enable_structural: bool,
    enable_deep_hooks: bool,
    sync_interval: u64,
    skip_initial_walk: bool,
}

/// Background task for transcript watcher.
///
/// Watches `~/.claude/projects/` for completed session transcripts and:
/// 1. Runs the ingest pipeline to extract lessons
/// 2. Runs the sync pipeline to update memory files
/// 3. Applies rate limiting (max 1 ingest per project per 5 minutes)
async fn run_transcript_watcher_task(db: Database, _data_dir: PathBuf, sync_interval_secs: u64) {
    use nellie::claude_code::daemon::{
        IngestRateLimiter, PeriodicSyncScheduler, TranscriptWatcher, TranscriptWatcherConfig,
    };
    use nellie::claude_code::ingest::{ingest_transcripts, IngestConfig};
    use nellie::claude_code::paths::resolve_claude_dir;
    use nellie::claude_code::sync::{execute_sync, SyncConfig};
    use nellie::storage::list_distinct_repos;

    // Resolve the claude projects directory
    let projects_dir = match resolve_claude_dir() {
        Ok(claude_dir) => claude_dir.join("projects"),
        Err(e) => {
            tracing::error!("Failed to resolve Claude projects directory: {e}");
            return;
        }
    };

    // Verify projects directory exists
    if !projects_dir.exists() {
        tracing::info!(
            path = %projects_dir.display(),
            "Claude projects directory does not exist (no transcripts to watch)"
        );
        return;
    }

    // Create transcript watcher
    let config = TranscriptWatcherConfig::new(projects_dir);
    let mut watcher = match TranscriptWatcher::new(config) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("Failed to create transcript watcher: {e}");
            return;
        }
    };

    // Start watching
    if let Err(e) = watcher.start() {
        tracing::error!("Failed to start transcript watcher: {e}");
        return;
    }

    tracing::info!("Transcript watcher task started");

    // Create rate limiter and periodic sync scheduler
    let rate_limiter = IngestRateLimiter::new();
    let sync_scheduler = PeriodicSyncScheduler::with_interval(sync_interval_secs);

    // Create periodic sync interval timer
    let mut sync_timer = tokio::time::interval(std::time::Duration::from_secs(sync_interval_secs));

    // Main watcher loop with periodic sync
    loop {
        tokio::select! {
            Some(transcript_path) = watcher.recv() => {
        tracing::info!(
            path = %transcript_path.display(),
            "Detected completed session transcript"
        );

        // Extract project directory from transcript path
        // Path is: ~/.claude/projects/<project-dir>/<session-id>.jsonl
        let project_path = if let Some(parent) = transcript_path.parent() {
            parent.to_path_buf()
        } else {
            tracing::error!(
                path = %transcript_path.display(),
                "Cannot extract project dir from transcript path"
            );
            continue;
        };

        // Check rate limit
        if !rate_limiter.should_ingest(&project_path) {
            tracing::info!(
                project = %project_path.display(),
                "Skipping transcript ingest (rate limited)"
            );
            continue;
        }

        // Run ingest pipeline
        let ingest_config = IngestConfig {
            transcript_path: Some(transcript_path.clone()),
            project_path: None,
            since: None,
            dry_run: false,
        };

        if let Err(e) = ingest_transcripts(&db, &ingest_config).and_then(|report| {
            tracing::info!(
                extracted = report.total_extracted,
                stored = report.total_stored,
                duplicates = report.total_duplicates,
                "Transcript ingestion completed"
            );

            // Run sync pipeline for the affected project
            // For deep hooks, we use the project_path itself as the working directory for sync
            let mut sync_config = SyncConfig::new(project_path.clone());
            sync_config.dry_run = false;

            execute_sync(&db, &sync_config).map(|sync_report| {
                tracing::info!(
                    lessons = sync_report.lessons_written,
                    checkpoints = sync_report.checkpoints_written,
                    rules = sync_report.rules_written,
                    "Memory sync completed after ingest"
                );
            })
        }) {
                tracing::error!(
                    error = %e,
                    path = %transcript_path.display(),
                    "Failed to ingest transcript or sync memory"
                );
            }
            }
            _ = sync_timer.tick() => {
                // Periodic sync: run sync for all known projects
                if sync_scheduler.should_sync() {
                    tracing::info!("Running periodic memory sync");

                    // Query distinct repositories from the database
                    let repos: Vec<String> = match db.with_conn(list_distinct_repos) {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("Failed to query repositories: {e}");
                            continue;
                        }
                    };

                    if repos.is_empty() {
                        tracing::debug!("No repositories to sync");
                    } else {
                        tracing::info!(repos = repos.len(), "Syncing repositories");

                        // Run sync for each known project
                        for repo in repos {
                            let mut sync_config = SyncConfig::new(std::path::PathBuf::from(&repo));
                            sync_config.dry_run = false;

                            match execute_sync(&db, &sync_config) {
                                Ok(report) => {
                                    tracing::info!(
                                        repo = %repo,
                                        lessons = report.lessons_written,
                                        checkpoints = report.checkpoints_written,
                                        rules = report.rules_written,
                                        "Periodic sync completed"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(repo = %repo, error = %e, "Periodic sync failed");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Serve command: Start the Nellie server
async fn serve_command(args: ServeCommandArgs) -> Result<()> {
    tracing::info!("Starting Nellie server...");

    // Build config from CLI arguments
    let config = Config {
        data_dir: args.data_dir.clone(),
        host: args.host.clone(),
        port: args.port,
        log_level: args.log_level,
        watch_dirs: args.watch.clone(),
        embedding_threads: args.embedding_threads,
        api_key: args.api_key.clone(),
        enable_structural: args.enable_structural,
    };

    tracing::debug!(?config, "Configuration loaded");

    // Validate config
    config.validate()?;

    tracing::info!(
        "Server binding to {}:{}, data directory: {:?}",
        args.host,
        args.port,
        config.data_dir
    );

    if args.api_key.is_some() {
        tracing::info!("API key authentication enabled");
    } else {
        tracing::warn!(
            "API key authentication DISABLED - server is accessible without credentials!"
        );
    }

    if args.disable_embeddings {
        tracing::warn!("Embeddings disabled - semantic search will not work");
    } else {
        tracing::info!(
            "Embedding service will be initialized (uses {} threads)",
            args.embedding_threads
        );
    }

    if args.enable_graph {
        tracing::info!("Nellie-V graph memory layer ENABLED");
    }

    if args.enable_structural {
        tracing::info!("Tree-sitter structural analysis ENABLED");
    }

    if args.enable_deep_hooks {
        tracing::info!(
            "Deep Hooks daemon ENABLED (auto-ingest + periodic sync every {} minutes)",
            args.sync_interval
        );
    }

    if !args.watch.is_empty() {
        tracing::info!("Watching directories: {:?}", args.watch);
    }

    // Initialize database
    let db = Database::open(config.database_path())?;
    init_storage(&db)?;

    // Initialize metrics
    init_metrics();

    // Startup ORT version check (issue #60).
    // Eagerly load the ONNX Runtime library and validate its version before
    // creating any Session.  On mismatch the error explains exactly what is
    // needed — no more silent crash-loops.
    if !args.disable_embeddings {
        match nellie::embeddings::version::check_ort_version() {
            Ok(build_info) => {
                tracing::info!(
                    min = nellie::embeddings::version::MIN_ORT_VERSION,
                    "ONNX Runtime loaded — {build_info}"
                );
            }
            Err(msg) => {
                tracing::error!("{msg}");
                std::process::exit(1);
            }
        }
    }

    // Create and run server
    let server_config = ServerConfig {
        host: args.host,
        port: args.port,
        shutdown_timeout: Duration::from_secs(30),
        api_key: args.api_key,
        data_dir: config.data_dir.clone(),
        embedding_threads: args.embedding_threads,
        enable_embeddings: !args.disable_embeddings,
        watch_dirs: args.watch.clone(),
        graph: nellie::config::GraphConfig {
            enabled: args.enable_graph,
            ..nellie::config::GraphConfig::default()
        },
        enable_structural: args.enable_structural,
    };

    // Clone db for the indexer before giving it to the App
    let indexer_db = db.clone();

    let app = App::new(server_config.clone(), db).await?;

    // Wire up transcript watcher for Deep Hooks if enabled
    if args.enable_deep_hooks {
        let db_deep = indexer_db.clone();
        let data_dir_deep = config.data_dir.clone();
        let sync_interval = args.sync_interval * 60; // Convert minutes to seconds
        tokio::spawn(async move {
            run_transcript_watcher_task(db_deep, data_dir_deep, sync_interval).await;
        });
    }

    // Wire up file watcher and indexer if watch dirs specified
    if !args.watch.is_empty() {
        // Share the App's embedding service instead of creating a second one.
        // Creating duplicate ONNX sessions causes the first session's workers
        // to die (ort global runtime state interference). See Issue #20.
        let embeddings = app.embeddings();

        let scan_db = indexer_db.clone();
        let indexer =
            std::sync::Arc::new(Indexer::new(indexer_db, embeddings, args.enable_structural));
        let (index_tx, index_rx) = tokio::sync::mpsc::channel::<IndexRequest>(1000);
        let (delete_tx, delete_rx) = tokio::sync::mpsc::channel(100);

        // Start the indexer loop
        let indexer_clone = std::sync::Arc::clone(&indexer);
        tokio::spawn(async move {
            indexer_clone.run(index_rx, delete_rx).await;
        });
        // Startup reconciliation: walk filesystem on startup or use DB-first mode.
        // For each known file, stat() it — if gone, delete from index; if changed, re-index.
        // New files are discovered by the watcher (FSEvents).
        let index_tx_scan = index_tx;
        let delete_tx_scan = delete_tx.clone();
        let skip_walk = args.skip_initial_walk;
        let watch_dirs = args.watch.clone();
        std::thread::spawn(move || {
            if skip_walk {
                tracing::info!("--skip-initial-walk set, using DB-first reconciliation only");
                reconcile_from_db(&scan_db, &index_tx_scan, &delete_tx_scan);
            } else if let Err(e) =
                reconcile_with_walk(&scan_db, &watch_dirs, &index_tx_scan, &delete_tx_scan)
            {
                tracing::error!(error = ?e, "Filesystem walk reconciliation failed");
            }
        });

        // Start file watcher for ongoing changes — uses direct indexer calls
        // to bypass the scan channel and get immediate indexing of new/changed files
        let watcher_watch_dirs = args.watch.clone();
        let watcher_indexer = std::sync::Arc::clone(&indexer);
        let watcher_delete_tx = delete_tx;
        tokio::spawn(async move {
            let watcher_config = WatcherConfig {
                watch_dirs: watcher_watch_dirs,
                ..Default::default()
            };
            match FileWatcher::new(&watcher_config) {
                Ok(mut watcher) => {
                    tracing::info!("File watcher started");
                    while let Some(batch) = watcher.recv().await {
                        let total = batch.modified.len() + batch.deleted.len();
                        tracing::info!(events = total, "Processing file change batch");

                        for path in batch.modified {
                            if FileFilter::is_code_file(&path) && !is_default_ignored_path(&path) {
                                let language = FileFilter::detect_language(&path).map(String::from);
                                let request = IndexRequest {
                                    path: path.clone(),
                                    language,
                                };
                                match watcher_indexer.index_file(&request).await {
                                    Ok(chunks) => {
                                        if chunks > 0 {
                                            tracing::info!(
                                                path = %path.display(),
                                                chunks,
                                                "Watcher indexed file"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            path = %path.display(),
                                            error = %e,
                                            "Watcher failed to index file"
                                        );
                                    }
                                }
                            }
                        }
                        for path in batch.deleted {
                            let _ = watcher_delete_tx.send(path).await;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to start file watcher: {e}");
                }
            }
        });
    }

    app.run().await
}

/// Statistics from filesystem walk reconciliation.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct WalkStats {
    /// Total files seen during the walk.
    seen: usize,
    /// Files queued for indexing.
    queued: usize,
    /// Files restaged (changed since last index).
    restaged: usize,
}

/// Walk the watch directories on startup, reconcile against the DB (respecting .gitignore),
/// and queue any new or stale files for indexing. Runs BEFORE the incremental watcher starts.
///
/// This is what users expect on a fresh install — point Nellie at an existing repo and have
/// it indexed automatically.
///
/// # Errors
///
/// Returns an error if the walk fails or if unable to compute file hashes.
#[allow(clippy::unnecessary_wraps)]
fn reconcile_with_walk(
    db: &Database,
    watch_dirs: &[PathBuf],
    index_tx: &tokio::sync::mpsc::Sender<IndexRequest>,
    delete_tx: &tokio::sync::mpsc::Sender<std::path::PathBuf>,
) -> Result<WalkStats> {
    use ignore::WalkBuilder;

    tracing::info!(
        "Starting filesystem walk reconciliation for {} dirs",
        watch_dirs.len()
    );
    let mut stats = WalkStats::default();

    for dir in watch_dirs {
        if !dir.exists() {
            tracing::warn!(path = ?dir, "Watch directory does not exist, skipping");
            continue;
        }

        let walker = WalkBuilder::new(dir)
            .standard_filters(true) // honors .gitignore, hidden, etc.
            .build();

        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            stats.seen += 1;

            if !FileFilter::is_code_file(path) || is_default_ignored_path(path) {
                continue;
            }

            // Read file and compute hash
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(path = ?path, error = %e, "Failed to read file");
                    continue;
                }
            };

            let file_hash = compute_hash(&content);
            let path_str = path.to_string_lossy();

            // Check if file is already indexed with the same hash
            let already_indexed = db
                .with_conn(
                    |conn| match nellie::storage::get_file_state(conn, &path_str) {
                        Ok(Some(file_state)) => Ok(file_state.hash == file_hash),
                        Ok(None) => Ok(false),
                        Err(e) => Err(e),
                    },
                )
                .unwrap_or(false);

            if already_indexed {
                stats.restaged += 1;
            } else {
                stats.queued += 1;
                let language = FileFilter::detect_language(path).map(String::from);
                let request = IndexRequest {
                    path: path.to_path_buf(),
                    language,
                };
                if index_tx.blocking_send(request).is_err() {
                    tracing::warn!("Index channel closed during walk reconciliation");
                    return Ok(stats);
                }
            }
        }
    }

    // After the walk, also run DB reconciliation to catch deleted files
    let paths = match db.with_conn(nellie::storage::list_file_paths) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list file paths from DB during walk");
            return Ok(stats);
        }
    };

    for path_str in paths {
        let path = PathBuf::from(&path_str);
        if !path.exists() && delete_tx.blocking_send(path).is_err() {
            tracing::warn!("Delete channel closed during walk reconciliation");
            return Ok(stats);
        }
    }

    tracing::info!(
        seen = stats.seen,
        queued = stats.queued,
        restaged = stats.restaged,
        "Filesystem walk reconciliation complete"
    );
    Ok(stats)
}

/// Compute blake3 hash of content.
fn compute_hash(content: &str) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Reconcile index state from DB on startup (no filesystem walk).
///
/// Instead of recursively walking NFS directories (which hangs on slow mounts),
/// iterate the `file_state` table and check each known file's metadata.
/// - If file is gone: delete from index
/// - If mtime or size changed: queue for re-indexing
/// - If unchanged: skip (fast path)
///
/// New files are discovered by the watcher (FSEvents), not the startup scan.
fn reconcile_from_db(
    db: &Database,
    index_tx: &tokio::sync::mpsc::Sender<IndexRequest>,
    delete_tx: &tokio::sync::mpsc::Sender<std::path::PathBuf>,
) {
    tracing::info!("Starting DB-first reconciliation (no filesystem walk)");

    let paths = match db.with_conn(nellie::storage::list_file_paths) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list file paths from DB");
            return;
        }
    };

    let total = paths.len();
    tracing::info!(tracked_files = total, "Reconciling file states");

    let mut unchanged = 0u64;
    let mut requeued = 0u64;
    let mut deleted = 0u64;
    let mut errors = 0u64;

    for (i, path_str) in paths.iter().enumerate() {
        let path = std::path::PathBuf::from(path_str);

        match std::fs::metadata(&path) {
            Ok(metadata) => {
                #[allow(clippy::cast_possible_wrap)]
                let mtime = metadata
                    .modified()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64
                    })
                    .unwrap_or(0);
                #[allow(clippy::cast_possible_wrap)]
                let size = metadata.len() as i64;

                let needs_index = db
                    .with_conn(|conn| {
                        nellie::storage::needs_reindex_by_metadata(conn, path_str, mtime, size)
                    })
                    .unwrap_or(true);

                if needs_index {
                    let language = FileFilter::detect_language(&path).map(String::from);
                    if index_tx
                        .blocking_send(IndexRequest { path, language })
                        .is_err()
                    {
                        tracing::warn!("Index channel closed during reconciliation");
                        return;
                    }
                    requeued += 1;
                } else {
                    unchanged += 1;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if delete_tx.blocking_send(path).is_err() {
                    tracing::warn!("Delete channel closed during reconciliation");
                    return;
                }
                deleted += 1;
            }
            Err(_) => {
                errors += 1;
            }
        }

        if (i + 1) % 10000 == 0 {
            tracing::info!(
                progress = i + 1,
                total,
                unchanged,
                requeued,
                deleted,
                errors,
                "Reconciliation progress..."
            );
        }
    }

    tracing::info!(
        total,
        unchanged,
        requeued,
        deleted,
        errors,
        "Reconciliation complete"
    );
}

/// Check if a path should be ignored (simplified version for scan).
fn is_default_ignored_path(path: &std::path::Path) -> bool {
    let path_str = path.to_string_lossy();

    // Dotdir heuristic
    for component in path_str.split('/') {
        if component.starts_with('.')
            && component.len() > 1
            && component != ".github"
            && component != ".gitignore"
        {
            return true;
        }
    }

    let ignored_dirs = [
        "node_modules",
        "target",
        "build",
        "dist",
        "__pycache__",
        "venv",
        "vendor",
        "obj",
        "bin",
        "coverage",
        "egg-info",
    ];

    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if ignored_dirs.contains(&name) {
            return true;
        }
    }

    false
}

/// Index command: Manually index directories via HTTP-first, local fallback.
///
/// When the server is reachable, routes through the HTTP API. Otherwise falls
/// back to local indexing which initializes an embedding service directly.
async fn index_command(
    paths: Vec<PathBuf>,
    server: String,
    force_local: bool,
    data_dir: PathBuf,
) -> Result<()> {
    if paths.is_empty() {
        return Err(nellie::Error::internal("nellie index: no paths provided"));
    }

    for path in &paths {
        if !path.exists() {
            return Err(nellie::Error::internal(format!(
                "Path does not exist: {}",
                path.display()
            )));
        }
    }

    // Prefer routing through the running server (avoids dual-DB writes).
    if !force_local {
        match try_index_via_server(&server, &paths).await {
            Ok(summary) => {
                println!(
                    "Indexing complete via {}: {} files, {} chunks, {:.1}s",
                    server, summary.files, summary.chunks, summary.elapsed_secs
                );
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "Could not reach server at {}, falling back to local indexing",
                    server
                );
            }
        }
    }

    // Local path: walk, embed, persist directly. Uses an exclusive DB lock.
    index_locally(&paths, &data_dir).await
}

/// Summary of indexing results.
#[derive(Debug)]
struct IndexSummary {
    files: usize,
    chunks: usize,
    elapsed_secs: f64,
}

/// Attempt to index via HTTP POST to the running server.
async fn try_index_via_server(server: &str, paths: &[PathBuf]) -> Result<IndexSummary> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| nellie::Error::internal(format!("Failed to create HTTP client: {e}")))?;

    let mut total_files = 0usize;
    let mut total_chunks = 0usize;
    let start = std::time::Instant::now();

    for path in paths {
        let body = serde_json::json!({
            "name": "index_repo",
            "arguments": { "path": path.to_string_lossy() }
        });
        let resp = client
            .post(format!("{}/mcp/invoke", server.trim_end_matches('/')))
            .json(&body)
            .send()
            .await
            .map_err(|e| nellie::Error::internal(format!("HTTP request failed: {e}")))?
            .error_for_status()
            .map_err(|e| nellie::Error::internal(format!("Server returned error: {e}")))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| nellie::Error::internal(format!("Failed to parse response: {e}")))?;

        if let Some(files) = resp
            .pointer("/content/files_indexed")
            .and_then(serde_json::Value::as_u64)
        {
            total_files += files as usize;
        }
        if let Some(chunks) = resp
            .pointer("/content/chunks_created")
            .and_then(serde_json::Value::as_u64)
        {
            total_chunks += chunks as usize;
        }
    }

    Ok(IndexSummary {
        files: total_files,
        chunks: total_chunks,
        elapsed_secs: start.elapsed().as_secs_f64(),
    })
}

/// Index locally by walking paths and calling the Indexer directly.
///
/// Initializes the embedding service so files are actually chunked and embedded.
///
/// WARNING: This function assumes the server is NOT running. Concurrent writes
/// to the database will result in corruption. Only use --local when the server
/// is down.
async fn index_locally(paths: &[PathBuf], data_dir: &Path) -> Result<()> {
    use ignore::WalkBuilder;
    use nellie::embeddings::{EmbeddingConfig, EmbeddingService};

    let config = Config {
        data_dir: data_dir.to_path_buf(),
        ..Config::default()
    };
    let db = Database::open(config.database_path())?;
    init_storage(&db)?;

    // Validate ONNX Runtime version before creating any Session (issue #60).
    match nellie::embeddings::version::check_ort_version() {
        Ok(build_info) => {
            tracing::info!(
                min = nellie::embeddings::version::MIN_ORT_VERSION,
                "ONNX Runtime loaded — {build_info}"
            );
        }
        Err(msg) => {
            return Err(nellie::Error::internal(msg));
        }
    }

    // Initialize the embedding service so chunks actually get embeddings.
    let emb_config = EmbeddingConfig::from_data_dir(data_dir, 4);
    if !emb_config.model_path.exists() {
        return Err(nellie::Error::internal(format!(
            "Embedding model not found at {}. \
             Run `nellie setup` or start the server first.",
            emb_config.model_path.display()
        )));
    }
    if !emb_config.tokenizer_path.exists() {
        return Err(nellie::Error::internal(format!(
            "Tokenizer not found at {}. \
             Run `nellie setup` or start the server first.",
            emb_config.tokenizer_path.display()
        )));
    }

    let embedding_service = EmbeddingService::new(emb_config);
    embedding_service.init().await.map_err(|e| {
        nellie::Error::internal(format!(
            "Failed to initialize embedding service: {e}. \
             Ensure ONNX Runtime is installed \
             (set ORT_DYLIB_PATH or run `nellie setup`)."
        ))
    })?;
    tracing::info!("Embedding service initialized for local indexing");

    let indexer = Indexer::new(db.clone(), Some(embedding_service), false);

    let mut total_files = 0usize;
    let mut total_chunks = 0usize;
    let start = std::time::Instant::now();

    for path in paths {
        let walker = WalkBuilder::new(path).standard_filters(true).build();
        for entry in walker.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            // Detect language and create index request
            let language = FileFilter::detect_language(p).map(String::from);
            let request = IndexRequest {
                path: p.to_path_buf(),
                language,
            };
            match indexer.index_file(&request).await {
                Ok(chunks) => {
                    if chunks > 0 {
                        total_files += 1;
                        total_chunks += chunks;
                    }
                }
                Err(e) => {
                    tracing::warn!(path = ?p, error = ?e, "Local index failed for file");
                }
            }
        }
    }

    println!(
        "Local indexing complete: {} files, {} chunks in {:.1}s",
        total_files,
        total_chunks,
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Search command: Perform semantic search
#[allow(clippy::needless_pass_by_value)]
fn search_command(query: String, limit: usize, threshold: f32, server: String) -> Result<()> {
    tracing::info!(
        "Searching for: '{}' (limit={}, threshold={})",
        query,
        limit,
        threshold
    );

    // Open database directly and get statistics
    let db = Database::open(Config::default().database_path())?;

    // Initialize storage schema if needed
    init_storage(&db)?;

    let chunk_count = db.with_conn(nellie::storage::count_chunks)?;

    if chunk_count == 0 {
        println!("No indexed chunks found in database.");
        println!("Please index code first using: nellie index <path>");
        return Ok(());
    }

    // For semantic search, we would need embeddings. Since search requires the
    // embedding worker (which needs async context and the server running),
    // we direct the user to use the server's search API.
    println!("Semantic code search for: {query}");
    println!("  Limit: {limit}");
    println!("  Threshold: {threshold}");
    println!("  Server: {server}");
    println!();
    println!("Note: Semantic search requires the server to be running.");
    println!("Start the server with: nellie serve");
    println!();
    println!("Then query it via the MCP API or REST endpoint:");
    println!("  - MCP Tool: search_code");
    println!("  - REST: POST {server}/api/v1/search/code");
    println!();
    println!("Database contains {chunk_count} indexed chunks ready for search.");

    Ok(())
}

/// Sync command: Sync Nellie knowledge to Claude Code memory files
#[allow(clippy::too_many_arguments)]
async fn sync_command(
    data_dir: PathBuf,
    project: Option<PathBuf>,
    dry_run: bool,
    max_lessons: usize,
    max_checkpoints: usize,
    budget: usize,
    rules: bool,
    server: Option<String>,
) -> Result<()> {
    // Resolve the project directory (default to CWD)
    let project_dir = match project {
        Some(p) => {
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir()
                    .map_err(|e| nellie::Error::internal(format!("cannot get CWD: {e}")))?
                    .join(p)
            }
        }
        None => std::env::current_dir()
            .map_err(|e| nellie::Error::internal(format!("cannot get CWD: {e}")))?,
    };

    tracing::info!(
        project = %project_dir.display(),
        dry_run,
        max_lessons,
        max_checkpoints,
        budget,
        rules,
        server = server.as_deref().unwrap_or("local"),
        "Starting Nellie sync to Claude Code memory files"
    );

    // Build sync config
    let mut sync_config = nellie::claude_code::sync::SyncConfig::new(project_dir);
    sync_config.dry_run = dry_run;
    sync_config.max_lessons = max_lessons;
    sync_config.max_checkpoints = max_checkpoints;
    sync_config.budget = budget;
    sync_config.sync_rules = rules;

    // Execute sync — remote or local
    let report = if let Some(ref server_url) = server {
        let client = nellie::claude_code::remote::RemoteClient::new(server_url);
        nellie::claude_code::sync::execute_sync_remote(&client, &sync_config).await?
    } else {
        let config = nellie::Config {
            data_dir,
            ..nellie::Config::default()
        };
        let db = Database::open(config.database_path())?;
        init_storage(&db)?;
        nellie::claude_code::sync::execute_sync(&db, &sync_config)?
    };

    // Print report
    nellie::claude_code::sync::print_report(&report, dry_run);

    // Write timestamp marker (skip on dry-run)
    if !dry_run {
        if let Err(e) = nellie::claude_code::paths::write_state_timestamp(
            &sync_config.project_dir,
            "last_sync_time",
        ) {
            eprintln!("Warning: failed to write sync timestamp: {e}");
        }
    }

    Ok(())
}

/// Parses a time-ago string or Unix timestamp into a Unix timestamp.
///
/// Supports formats:
/// - Unix timestamp: "1630000000"
/// - Minutes: "30m"
/// - Hours: "1h"
/// - Days: "2d"
/// - Weeks: "1w"
///
/// Returns the Unix timestamp for the point in time relative to now.
fn parse_time_since(since_str: &str) -> Result<i64> {
    // Try parsing as Unix timestamp first
    if let Ok(ts) = since_str.parse::<i64>() {
        return Ok(ts);
    }

    let since_str = since_str.trim().to_lowercase();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| nellie::Error::internal(format!("system time error: {e}")))?
            .as_secs(),
    )
    .unwrap_or(i64::MAX);

    // Parse human-readable time formats
    if let Some(digits) = since_str.strip_suffix('m') {
        let minutes: i64 = digits
            .parse()
            .map_err(|_| nellie::Error::internal(format!("invalid time format: {since_str}")))?;
        return Ok(now - (minutes * 60));
    }

    if let Some(digits) = since_str.strip_suffix('h') {
        let hours: i64 = digits
            .parse()
            .map_err(|_| nellie::Error::internal(format!("invalid time format: {since_str}")))?;
        return Ok(now - (hours * 3600));
    }

    if let Some(digits) = since_str.strip_suffix('d') {
        let days: i64 = digits
            .parse()
            .map_err(|_| nellie::Error::internal(format!("invalid time format: {since_str}")))?;
        return Ok(now - (days * 86400));
    }

    if let Some(digits) = since_str.strip_suffix('w') {
        let weeks: i64 = digits
            .parse()
            .map_err(|_| nellie::Error::internal(format!("invalid time format: {since_str}")))?;
        return Ok(now - (weeks * 604_800));
    }

    Err(nellie::Error::internal(
        format!("invalid --since format: '{since_str}'. Use Unix timestamp or human-readable format like '1h', '2d', '30m', '1w'"),
    ))
}

/// Ingest command: Parse and extract lessons from transcripts
#[allow(clippy::needless_pass_by_value)]
async fn ingest_command(
    data_dir: PathBuf,
    transcript: Option<PathBuf>,
    project: Option<PathBuf>,
    since: Option<String>,
    dry_run: bool,
    server: Option<String>,
) -> Result<()> {
    use nellie::claude_code::ingest::{
        ingest_transcripts, ingest_transcripts_remote, IngestConfig,
    };

    // Validate arguments
    if transcript.is_some() && project.is_some() {
        return Err(nellie::Error::internal(
            "cannot specify both --transcript and --project",
        ));
    }

    if transcript.is_none() && project.is_none() {
        return Err(nellie::Error::internal(
            "must specify either a transcript path or --project",
        ));
    }

    if since.is_some() && project.is_none() {
        return Err(nellie::Error::internal(
            "--since can only be used with --project (batch mode)",
        ));
    }

    // Parse --since time format (human-readable or Unix timestamp)
    let since_timestamp = if let Some(ref since_str) = since {
        Some(parse_time_since(since_str)?)
    } else {
        None
    };

    tracing::info!(
        transcript = ?transcript,
        project = ?project,
        since = ?since,
        since_timestamp = ?since_timestamp,
        dry_run,
        server = server.as_deref().unwrap_or("local"),
        "Starting transcript ingestion"
    );

    // Build ingest config
    let ingest_config = IngestConfig {
        transcript_path: transcript,
        project_path: project,
        since: since_timestamp,
        dry_run,
    };

    // Execute ingest — remote or local
    let report = if let Some(ref server_url) = server {
        let client = nellie::claude_code::remote::RemoteClient::new(server_url);
        ingest_transcripts_remote(&client, &ingest_config).await?
    } else {
        let config = nellie::Config {
            data_dir,
            ..nellie::Config::default()
        };
        let db = Database::open(config.database_path())?;
        init_storage(&db)?;
        ingest_transcripts(&db, &ingest_config)?
    };

    // Print report
    print_ingest_report(&report, dry_run);

    // Write timestamp marker (skip on dry-run)
    if !dry_run {
        let project_dir = ingest_config
            .project_path
            .as_deref()
            .or_else(|| {
                ingest_config
                    .transcript_path
                    .as_deref()
                    .and_then(|p| p.parent())
            })
            .unwrap_or_else(|| Path::new("."));
        let project_dir = if project_dir.is_absolute() {
            project_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(project_dir)
        };
        if let Err(e) =
            nellie::claude_code::paths::write_state_timestamp(&project_dir, "last_ingest_time")
        {
            eprintln!("Warning: failed to write ingest timestamp: {e}");
        }
    }

    Ok(())
}

/// Print ingestion report in human-readable format
fn print_ingest_report(report: &nellie::claude_code::ingest::IngestReport, dry_run: bool) {
    println!();
    println!("=== Transcript Ingestion Report ===");
    println!();

    if dry_run {
        println!("MODE: DRY RUN (no lessons actually stored)");
        println!();
    }

    println!("Summary:");
    println!("  Transcripts processed:  {}", report.transcripts_processed);
    println!("  Lessons extracted:      {}", report.total_extracted);
    println!("  New lessons stored:     {}", report.total_stored);
    println!("  Duplicate lessons:      {}", report.total_duplicates);

    if !report.transcript_reports.is_empty() {
        println!();
        println!("Per-Transcript Details:");
        for tr in &report.transcript_reports {
            println!();
            println!("  {}", tr.path);
            println!("    Extracted: {}", tr.extracted_count);
            println!("    New:       {}", tr.new_lessons);
            println!("    Duplicate: {}", tr.duplicate_lessons);
            if !tr.stored_lessons.is_empty() {
                println!("    Stored lessons:");
                for title in &tr.stored_lessons {
                    println!("      - {title}");
                }
            }
        }
    }

    if !report.errors.is_empty() {
        println!();
        println!("Errors (non-fatal):");
        for error in &report.errors {
            println!("  - {error}");
        }
    }

    println!();
}

/// Status command: Show server status
#[allow(clippy::needless_pass_by_value)]
fn status_command(_server: String, format: String) -> Result<()> {
    // Open database directly and get statistics
    let db = Database::open(Config::default().database_path())?;

    // Initialize storage schema if needed
    init_storage(&db)?;

    let chunk_count = db.with_conn(nellie::storage::count_chunks)?;
    let lesson_count = db.with_conn(nellie::storage::count_lessons)?;
    let file_count = db.with_conn(nellie::storage::count_tracked_files)?;

    tracing::info!(
        "Status: {} chunks, {} lessons, {} tracked files",
        chunk_count,
        lesson_count,
        file_count
    );

    if format == "json" {
        // JSON output
        let json = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "stats": {
                "indexed_chunks": chunk_count,
                "lessons": lesson_count,
                "tracked_files": file_count
            }
        });
        let json_str = serde_json::to_string_pretty(&json)
            .map_err(|e| nellie::Error::internal(format!("JSON serialization error: {e}")))?;
        println!("{json_str}");
    } else {
        // Text output (default)
        println!("Nellie Production v{}", env!("CARGO_PKG_VERSION"));
        println!();
        println!("Status:");
        println!("  Indexed chunks:  {chunk_count}");
        println!("  Lessons:         {lesson_count}");
        println!("  Tracked files:   {file_count}");
    }

    Ok(())
}

/// Inject command: Inject Nellie context for the current prompt
#[allow(clippy::too_many_arguments, clippy::unnecessary_wraps)]
async fn inject_command(
    query: &str,
    limit: usize,
    threshold: f64,
    timeout_ms: u64,
    server: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    use nellie::claude_code::inject::{execute_inject, InjectConfig};

    let start = std::time::Instant::now();

    // Build inject config
    let config = InjectConfig {
        query: query.to_string(),
        limit,
        threshold,
        timeout_ms,
        dry_run,
    };

    tracing::info!(
        query = %config.query,
        limit = config.limit,
        threshold = config.threshold,
        timeout_ms = config.timeout_ms,
        dry_run = config.dry_run,
        server = server.unwrap_or("local"),
        "Starting context injection"
    );

    // Run the injection pipeline
    let result = execute_inject(&config, server).await?;

    let elapsed = start.elapsed();
    let total_ms = elapsed.as_millis() as u64;

    if dry_run {
        println!("=== Dry Run ===");
        println!("Query: {}", config.query);
        println!("Limit: {}", config.limit);
        println!("Threshold: {}", config.threshold);
        println!("Timeout: {}ms", config.timeout_ms);
        println!("Server: {}", server.unwrap_or("local"));
        println!();
        println!(
            "Results: {} injected, {} skipped",
            result.injected_count, result.skipped_count
        );
        println!("Elapsed: {total_ms}ms");
    } else {
        if result.injected_count > 0 {
            println!(
                "Injected {} lessons ({} skipped, threshold={})",
                result.injected_count, result.skipped_count, threshold
            );
            if let Some(path) = &result.file_path {
                println!("File: {path}");
            }
        } else {
            println!("No relevant context found (threshold={threshold})");
        }
        tracing::info!(
            injected = result.injected_count,
            skipped = result.skipped_count,
            elapsed_ms = total_ms,
            "Injection complete"
        );

        // Write last_inject_time timestamp for hooks-status
        let cwd = std::env::current_dir().unwrap_or_default();
        let _ = nellie::claude_code::paths::write_state_timestamp(&cwd, "last_inject_time");
    }

    Ok(())
}

/// Hooks Install command: Install Nellie hooks to Claude Code settings.json
fn hooks_install_command(force: bool, server: Option<&str>) -> Result<()> {
    use nellie::claude_code::hooks::install_hooks;

    tracing::info!(
        "Installing Nellie hooks to Claude Code settings.json{}",
        if force {
            " (force mode: replacing old shell hooks)"
        } else {
            ""
        }
    );

    // Check if nellie is on PATH
    if std::process::Command::new("nellie")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("Warning: 'nellie' is not on PATH. Hooks will fail to run.");
        eprintln!("  Fix: ln -sf /path/to/nellie ~/.local/bin/nellie");
        eprintln!();
    }

    let report = install_hooks(force, server)?;

    println!();
    println!("=== Hook Installation Report ===");
    println!();
    println!("Settings file: {}", report.settings_path.display());
    println!();

    if let Some((hook_name, _cmd)) = &report.old_shell_hook_info {
        println!("Legacy hook detected and removed:");
        println!("  ⚠ {hook_name}");
        println!();
    }

    println!("Hooks installed:");
    if report.session_start_installed {
        let server_info = server.map_or(String::new(), |s| format!(" --server {s}"));
        println!("  ✓ SessionStart (nellie sync --project \"$PWD\" --rules{server_info})");
    }
    if report.stop_installed {
        let server_info = server.map_or(String::new(), |s| format!(" --server {s}"));
        println!("  ✓ Stop (nellie ingest --project \"$PWD\" --since 1h{server_info})");
    }
    if report.user_prompt_submit_installed {
        let server_info = server.map_or(String::new(), |s| format!(" --server {s}"));
        println!(
            "  ✓ UserPromptSubmit (nellie inject --query \"$CC_USER_PROMPT\" --limit 3{server_info})"
        );
    }
    if report.backup_created {
        println!();
        println!("Backup created: {}.bak", report.settings_path.display());
    }

    if report.old_shell_hook_replaced {
        println!();
        println!("Migration complete: Legacy shell hook replaced with native Nellie commands");
    }

    // Create memory directory upfront
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Ok(memory_dir) = nellie::claude_code::paths::resolve_project_memory_dir(&cwd) {
        let _ = std::fs::create_dir_all(&memory_dir);
    }

    // Run initial sync (use arg-vector form to avoid shell injection)
    println!();
    println!("Running initial sync...");
    let mut cmd = std::process::Command::new("nellie");
    cmd.arg("sync").arg("--rules");
    if let Some(url) = server {
        cmd.arg("--server").arg(url);
    }
    let sync_display = format!(
        "nellie sync --rules{}",
        server.map_or_else(String::new, |u| format!(" --server {u}"))
    );
    match cmd.status() {
        Ok(status) if status.success() => {
            println!("  ✓ Initial sync complete");
        }
        Ok(status) => {
            eprintln!(
                "  ✗ Initial sync failed (exit {}). Run manually: {sync_display}",
                status.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            eprintln!("  ✗ Could not run initial sync: {e}. Run manually: {sync_display}");
        }
    }

    println!();

    Ok(())
}

/// Hooks Uninstall command: Remove Nellie hooks from Claude Code settings.json
fn hooks_uninstall_command() -> Result<()> {
    use nellie::claude_code::hooks::uninstall_hooks;

    tracing::info!("Uninstalling Nellie hooks from Claude Code settings.json");
    uninstall_hooks()?;

    println!();
    println!("=== Hook Uninstall Report ===");
    println!();
    let settings_path = nellie::claude_code::paths::resolve_settings_path()?;
    println!("Nellie hooks removed from: {}", settings_path.display());
    println!("Existing hooks preserved");
    println!();

    Ok(())
}

/// Hooks Status command: Check the status of Nellie hooks
fn hooks_status_command(json: bool) -> Result<()> {
    use nellie::claude_code::hooks::check_hook_status;

    // Only emit tracing logs when not in JSON mode to keep output clean
    if !json {
        tracing::info!("Checking Nellie hooks status");
    }
    let status = check_hook_status()?;

    if json {
        println!("{}", status.format_json());
    } else {
        println!();
        println!("{}", status.format_text());
        println!();
    }

    Ok(())
}

/// Run the `nellie setup` command: download ORT, model, and tokenizer.
async fn setup_command(data_dir: &Path, skip_runtime: bool, skip_model: bool) -> Result<()> {
    nellie::setup::run_setup(data_dir, skip_runtime, skip_model)
        .await
        .map_err(|e| nellie::Error::internal(e.to_string()))?;
    Ok(())
}

/// Run the bootstrap command: import starter lessons into the database.
async fn bootstrap_command(data_dir: &Path, force: bool) -> Result<()> {
    // Ensure data directory exists
    std::fs::create_dir_all(data_dir).map_err(|e| {
        nellie::Error::internal(format!(
            "Failed to create data directory {}: {e}",
            data_dir.display()
        ))
    })?;

    let config = Config {
        data_dir: data_dir.to_path_buf(),
        ..Config::default()
    };
    let db = Database::open(config.database_path())?;
    init_storage(&db)?;

    tracing::info!(
        data_dir = %data_dir.display(),
        force,
        "Starting bootstrap"
    );

    let result = nellie::bootstrap::run_bootstrap(&db, data_dir, force).await?;

    // Print summary to stdout (this is a CLI command, not library code)
    println!(
        "Bootstrapped {} lessons ({} skipped, already present)",
        result.imported, result.skipped
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parsing_serve() {
        let args = vec!["nellie", "serve", "--host", "0.0.0.0", "--port", "9000"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Serve {
            host,
            port,
            watch,
            embedding_threads,
            disable_embeddings,
            enable_graph,
            enable_structural,
            enable_deep_hooks,
            sync_interval,
            skip_initial_walk,
        }) = cli.command
        {
            assert_eq!(host, "0.0.0.0");
            assert_eq!(port, 9000);
            assert!(watch.is_empty());
            assert_eq!(embedding_threads, 4);
            assert!(!disable_embeddings);
            assert!(!enable_graph);
            assert!(!enable_structural);
            assert!(!enable_deep_hooks);
            assert_eq!(sync_interval, 30);
            assert!(!skip_initial_walk);
        } else {
            panic!("Expected Serve command");
        }
    }

    #[test]
    fn test_cli_parsing_index() {
        let args = vec!["nellie", "index", "/path/to/code"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Index {
            paths,
            server,
            local,
        }) = cli.command
        {
            assert_eq!(paths.len(), 1);
            assert_eq!(server, "http://127.0.0.1:8765");
            assert!(!local);
        } else {
            panic!("Expected Index command");
        }
    }

    #[test]
    fn test_cli_parsing_search() {
        let args = vec!["nellie", "search", "find auth handler"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Search {
            query,
            limit,
            threshold,
            server,
        }) = cli.command
        {
            assert_eq!(query, "find auth handler");
            assert_eq!(limit, 10);
            assert_eq!(threshold, 0.5);
            assert_eq!(server, "http://127.0.0.1:8765");
        } else {
            panic!("Expected Search command");
        }
    }

    #[test]
    fn test_cli_parsing_status() {
        let args = vec!["nellie", "status"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Status { server, format }) = cli.command {
            assert_eq!(server, "http://127.0.0.1:8765");
            assert_eq!(format, "text");
        } else {
            panic!("Expected Status command");
        }
    }

    #[test]
    fn test_cli_global_options() {
        let args = vec![
            "nellie",
            "--data-dir",
            "/custom/data",
            "--log-level",
            "debug",
            "serve",
        ];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        assert_eq!(cli.data_dir, PathBuf::from("/custom/data"));
        assert_eq!(cli.log_level, "debug");
    }

    #[test]
    fn test_cli_json_logging() {
        let args = vec!["nellie", "--log-json", "serve"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        assert!(cli.log_json);
    }

    #[test]
    fn test_cli_search_with_options() {
        let args = vec![
            "nellie",
            "search",
            "database query",
            "--limit",
            "20",
            "--threshold",
            "0.7",
            "--server",
            "http://custom.server:9000",
        ];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Search {
            query,
            limit,
            threshold,
            server,
        }) = cli.command
        {
            assert_eq!(query, "database query");
            assert_eq!(limit, 20);
            assert_eq!(threshold, 0.7);
            assert_eq!(server, "http://custom.server:9000");
        } else {
            panic!("Expected Search command");
        }
    }

    #[test]
    fn test_cli_disable_embeddings() {
        let args = vec!["nellie", "serve", "--disable-embeddings"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Serve {
            disable_embeddings, ..
        }) = cli.command
        {
            assert!(disable_embeddings);
        } else {
            panic!("Expected Serve command");
        }
    }

    #[test]
    fn test_cli_parsing_sync_defaults() {
        let args = vec!["nellie", "sync"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Sync {
            project,
            dry_run,
            max_lessons,
            max_checkpoints,
            rules,
            budget,
            server: _,
        }) = cli.command
        {
            assert!(project.is_none());
            assert!(!dry_run);
            assert_eq!(max_lessons, 50);
            assert_eq!(max_checkpoints, 3);
            assert!(!rules);
            assert_eq!(budget, 180);
        } else {
            panic!("Expected Sync command");
        }
    }

    #[test]
    fn test_cli_parsing_sync_with_rules() {
        let args = vec!["nellie", "sync", "--rules"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Sync { rules, .. }) = cli.command {
            assert!(rules);
        } else {
            panic!("Expected Sync command");
        }
    }

    #[test]
    fn test_cli_parsing_sync_all_flags() {
        let args = vec![
            "nellie",
            "sync",
            "--project",
            "/tmp/myproject",
            "--dry-run",
            "--max-lessons",
            "25",
            "--max-checkpoints",
            "5",
            "--rules",
            "--budget",
            "150",
        ];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Sync {
            project,
            dry_run,
            max_lessons,
            max_checkpoints,
            rules,
            budget,
            server: _,
        }) = cli.command
        {
            assert_eq!(project, Some(PathBuf::from("/tmp/myproject")));
            assert!(dry_run);
            assert_eq!(max_lessons, 25);
            assert_eq!(max_checkpoints, 5);
            assert!(rules);
            assert_eq!(budget, 150);
        } else {
            panic!("Expected Sync command");
        }
    }

    #[test]
    fn test_cli_parsing_setup_defaults() {
        let args = vec!["nellie", "setup"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Setup {
            skip_runtime,
            skip_model,
        }) = cli.command
        {
            assert!(!skip_runtime);
            assert!(!skip_model);
            // data_dir comes from the global Cli.data_dir (with default)
            assert!(!cli.data_dir.as_os_str().is_empty());
        } else {
            panic!("Expected Setup command");
        }
    }

    #[test]
    fn test_cli_parsing_setup_all_flags() {
        let args = vec![
            "nellie",
            "setup",
            "--data-dir",
            "/tmp/nellie-data",
            "--skip-runtime",
            "--skip-model",
        ];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Setup {
            skip_runtime,
            skip_model,
        }) = cli.command
        {
            // --data-dir is a global arg on Cli, accessible via cli.data_dir
            assert_eq!(cli.data_dir, PathBuf::from("/tmp/nellie-data"));
            assert!(skip_runtime);
            assert!(skip_model);
        } else {
            panic!("Expected Setup command");
        }
    }

    #[test]
    fn test_cli_parsing_bootstrap_defaults() {
        let args = vec!["nellie", "bootstrap"];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Bootstrap { force }) = cli.command {
            assert!(!force);
        } else {
            panic!("Expected Bootstrap command");
        }
    }

    #[test]
    fn test_cli_parsing_bootstrap_force() {
        let args = vec![
            "nellie",
            "bootstrap",
            "--data-dir",
            "/tmp/nellie-data",
            "--force",
        ];
        let cli = Cli::try_parse_from(args);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        if let Some(Commands::Bootstrap { force }) = cli.command {
            assert_eq!(cli.data_dir, PathBuf::from("/tmp/nellie-data"));
            assert!(force);
        } else {
            panic!("Expected Bootstrap command");
        }
    }

    #[test]
    fn test_cli_help_message() {
        // Test that help parsing doesn't crash
        let args = vec!["nellie", "--help"];
        let _cli = Cli::try_parse_from(args);
        // --help causes exit, so we just verify parsing doesn't panic
        // Real test would need to capture output
    }

    #[test]
    fn test_walk_stats_default() {
        let stats = WalkStats::default();
        assert_eq!(stats.seen, 0);
        assert_eq!(stats.queued, 0);
        assert_eq!(stats.restaged, 0);
    }

    #[test]
    fn default_port_matches_config_example() {
        const CONFIG_EXAMPLE: &str = include_str!("../config.example.yaml");
        // Parse enough of the YAML to find the `port:` line
        let port_line = CONFIG_EXAMPLE
            .lines()
            .find(|l| l.trim_start().starts_with("port:"))
            .expect("config.example.yaml missing port:");
        assert!(
            port_line.contains("8765"),
            "config.example.yaml port does not match CLI default 8765: {port_line}"
        );
    }

    #[test]
    fn readme_mentions_port_8765() {
        const README: &str = include_str!("../README.md");
        let count_8765 = README.matches("localhost:8765").count();
        let count_8080 = README.matches("localhost:8080").count();
        assert!(count_8765 >= 1, "README should reference localhost:8765");
        assert_eq!(count_8080, 0, "README still references localhost:8080");
    }
}
