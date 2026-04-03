//! Bootstrap: seed graph from existing lessons and checkpoints.

use crate::graph::enrichment::{ensure_edge, ensure_entity};
use crate::graph::entities::{EntityType, RelationshipKind};
use crate::graph::memory::GraphMemory;
use crate::storage::{CheckpointRecord, LessonRecord};

/// Stats from a bootstrap run.
#[derive(Debug, Default)]
pub struct BootstrapStats {
    pub lessons_processed: u32,
    pub checkpoints_processed: u32,
    pub nodes_created: u32,
    pub edges_created: u32,
}

/// Process a single lesson into graph entities and edges.
///
/// Creates:
/// - A Solution node for the lesson itself (keyed by lesson ID)
/// - A Concept node for each tag + "related_to" edge to the solution
/// - Parses severity: if "critical" or "warning", infers a Problem node
pub fn process_lesson(graph: &mut GraphMemory, lesson: &LessonRecord) -> (u32, u32) {
    let mut nodes = 0u32;
    let mut edges = 0u32;

    // Create solution node for the lesson
    let solution_id = ensure_entity(graph, EntityType::Solution, &lesson.title);
    nodes += 1;

    // Each tag becomes a Concept node with related_to edge
    for tag in &lesson.tags {
        let tag_trimmed = tag.trim();
        if !tag_trimmed.is_empty() {
            let concept_id = ensure_entity(graph, EntityType::Concept, tag_trimmed);
            ensure_edge(
                graph,
                &solution_id,
                &concept_id,
                RelationshipKind::RelatedTo,
                Some("Bootstrapped from lesson tag".to_string()),
            );
            nodes += 1;
            edges += 1;
        }
    }

    // If severity is critical/warning, create a Problem node inferred from title
    if lesson.severity == "critical" || lesson.severity == "warning" {
        let problem_label = format!("[{}] {}", lesson.severity, lesson.title);
        let problem_id = ensure_entity(graph, EntityType::Problem, &problem_label);
        ensure_edge(
            graph,
            &solution_id,
            &problem_id,
            RelationshipKind::Solved,
            Some("Inferred: lesson with high severity likely solves a problem".to_string()),
        );
        nodes += 1;
        edges += 1;
    }

    (nodes, edges)
}

/// Process a single checkpoint into graph entities and edges.
///
/// Creates:
/// - An Agent node for the checkpoint's agent field
/// - A Chunk node referencing the checkpoint ID
/// - "related_to" edge from agent to checkpoint chunk
pub fn process_checkpoint(graph: &mut GraphMemory, checkpoint: &CheckpointRecord) -> (u32, u32) {
    let mut nodes = 0u32;
    let mut edges = 0u32;

    // Create agent node
    let agent_id = ensure_entity(graph, EntityType::Agent, &checkpoint.agent);
    nodes += 1;

    // Create chunk node for the checkpoint
    let chunk_id = ensure_entity(graph, EntityType::Chunk, &checkpoint.working_on);
    nodes += 1;

    // Agent → Chunk edge
    ensure_edge(
        graph,
        &agent_id,
        &chunk_id,
        RelationshipKind::RelatedTo,
        Some("Bootstrapped from checkpoint".to_string()),
    );
    edges += 1;

    // Try to extract concepts from working_on text (simple word tokenization)
    // Split on common delimiters and look for multi-word phrases
    let working_words: Vec<&str> = checkpoint
        .working_on
        .split([',', ';', ':', '|'])
        .map(str::trim)
        .filter(|w| w.len() > 2)
        .collect();

    for phrase in working_words.iter().take(5) {
        // Cap at 5 to avoid noise
        let concept_id = ensure_entity(graph, EntityType::Concept, phrase);
        ensure_edge(
            graph,
            &chunk_id,
            &concept_id,
            RelationshipKind::RelatedTo,
            Some("Extracted from checkpoint working_on".to_string()),
        );
        nodes += 1;
        edges += 1;
    }

    (nodes, edges)
}

/// Run bootstrap over all lessons and checkpoints.
///
/// If `dry_run` is true, operates on a cloned graph and discards changes.
/// Returns stats of what was (or would be) created.
pub fn run_bootstrap(
    graph: &mut GraphMemory,
    lessons: &[LessonRecord],
    checkpoints: &[CheckpointRecord],
) -> BootstrapStats {
    let mut stats = BootstrapStats::default();

    for lesson in lessons {
        let (n, e) = process_lesson(graph, lesson);
        stats.nodes_created += n;
        stats.edges_created += e;
        stats.lessons_processed += 1;
    }

    for checkpoint in checkpoints {
        let (n, e) = process_checkpoint(graph, checkpoint);
        stats.nodes_created += n;
        stats.edges_created += e;
        stats.checkpoints_processed += 1;
    }

    stats
}

/// Bootstrap structural graph from the `symbols` and `structural_edges` tables.
///
/// Reads all symbols, builds graph entities and edges, and returns stats.
///
/// # Errors
///
/// Returns an error if database queries fail.
pub fn bootstrap_structural(
    graph: &mut GraphMemory,
    db: &crate::storage::Database,
) -> crate::Result<crate::structural::graph_builder::StructuralGraphStats> {
    // Load all symbols
    let symbols: Vec<crate::structural::storage::SymbolRecord> = db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, symbol_name, symbol_kind, language, start_line, end_line, scope, signature, file_hash, indexed_at FROM symbols",
            )
            .map_err(|e| crate::error::StorageError::Database(format!("failed to prepare symbols query: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(crate::structural::storage::SymbolRecord {
                    id: row.get(0)?,
                    file_path: row.get(1)?,
                    symbol_name: row.get(2)?,
                    symbol_kind: crate::structural::SymbolKind::parse(&row.get::<_, String>(3)?).unwrap_or(crate::structural::SymbolKind::Function),
                    language: row.get(4)?,
                    start_line: row.get(5)?,
                    end_line: row.get(6)?,
                    scope: row.get(7)?,
                    signature: row.get(8)?,
                    file_hash: row.get(9)?,
                    indexed_at: row.get(10)?,
                })
            })
            .map_err(|e| crate::error::StorageError::Database(format!("failed to query symbols: {e}")))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| crate::error::StorageError::Database(format!("failed to read symbol: {e}")))?);
        }
        Ok(result)
    })?;

    // Load all structural edges
    let edges: Vec<(i64, String, Option<String>, String)> = db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT source_symbol_id, target_symbol_name, target_file_path, edge_kind FROM structural_edges",
            )
            .map_err(|e| crate::error::StorageError::Database(format!("failed to prepare edges query: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| crate::error::StorageError::Database(format!("failed to query edges: {e}")))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| crate::error::StorageError::Database(format!("failed to read edge: {e}")))?);
        }
        Ok(result)
    })?;

    tracing::info!(
        symbols = symbols.len(),
        edges = edges.len(),
        "Bootstrapping structural graph"
    );

    let stats = crate::structural::graph_builder::build_structural_graph(graph, &symbols, &edges);

    tracing::info!(
        nodes = stats.nodes_created,
        edges = stats.edges_created,
        files = stats.files_processed,
        "Structural graph bootstrap complete"
    );

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GraphConfig;

    fn make_lesson(title: &str, tags: Vec<&str>, severity: &str) -> LessonRecord {
        let mut lesson = LessonRecord::new(
            title,
            "Some content",
            tags.into_iter().map(String::from).collect(),
        );
        lesson.severity = severity.to_string();
        lesson
    }

    fn make_checkpoint(agent: &str, working_on: &str) -> CheckpointRecord {
        CheckpointRecord::new(agent, working_on, serde_json::json!({}))
    }

    #[test]
    fn test_process_lesson_creates_solution_and_concept_nodes() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        let lesson = make_lesson("Use reqwest for HTTP", vec!["rust", "http"], "info");
        let (nodes, edges) = process_lesson(&mut graph, &lesson);
        // Solution node + 2 concept nodes = 3 nodes, 2 edges (solution -> concept)
        assert!(nodes >= 3);
        assert!(edges >= 2);
    }

    #[test]
    fn test_process_lesson_critical_creates_problem_node() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        let lesson = make_lesson("Fix auth token expiry", vec!["auth"], "critical");
        let (nodes, edges) = process_lesson(&mut graph, &lesson);
        // Solution + concept(auth) + problem = 3+ nodes, concept edge + solved edge = 2+ edges
        assert!(nodes >= 3);
        assert!(edges >= 2);
    }

    #[test]
    fn test_process_checkpoint_creates_agent_and_chunk() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        let cp = make_checkpoint("test/my-project", "implementing graph layer");
        let (nodes, edges) = process_checkpoint(&mut graph, &cp);
        assert!(nodes >= 2); // agent + chunk
        assert!(edges >= 1); // agent -> chunk
    }

    #[test]
    fn test_run_bootstrap_processes_all() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        let lessons = vec![
            make_lesson("Lesson 1", vec!["tag1"], "info"),
            make_lesson("Lesson 2", vec!["tag2", "tag3"], "warning"),
        ];
        let checkpoints = vec![make_checkpoint("agent1", "working on task A")];
        let stats = run_bootstrap(&mut graph, &lessons, &checkpoints);
        assert_eq!(stats.lessons_processed, 2);
        assert_eq!(stats.checkpoints_processed, 1);
        assert!(stats.nodes_created > 0);
        assert!(stats.edges_created > 0);
    }

    #[test]
    fn test_bootstrap_idempotent() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        let lessons = vec![make_lesson("Same lesson", vec!["tag"], "info")];
        let _stats1 = run_bootstrap(&mut graph, &lessons, &[]);
        let node_count_after_first = graph.node_count();
        // Run again — should reuse existing nodes
        let _stats2 = run_bootstrap(&mut graph, &lessons, &[]);
        assert_eq!(
            graph.node_count(),
            node_count_after_first,
            "bootstrap should be idempotent"
        );
    }

    #[test]
    fn test_bootstrap_structural() {
        use crate::storage::Database;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let db_path = temp_dir.path().join("test.db");
        let db = Database::open(db_path.as_path()).expect("failed to create database");

        // Initialize storage schema
        db.with_conn(|conn: &rusqlite::Connection| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS symbols (
                    id INTEGER PRIMARY KEY,
                    file_path TEXT NOT NULL,
                    symbol_name TEXT NOT NULL,
                    symbol_kind TEXT NOT NULL,
                    language TEXT NOT NULL,
                    start_line INTEGER NOT NULL,
                    end_line INTEGER NOT NULL,
                    scope TEXT,
                    signature TEXT,
                    file_hash TEXT NOT NULL,
                    indexed_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS structural_edges (
                    id INTEGER PRIMARY KEY,
                    source_symbol_id INTEGER NOT NULL,
                    target_symbol_name TEXT NOT NULL,
                    target_file_path TEXT,
                    edge_kind TEXT NOT NULL,
                    FOREIGN KEY(source_symbol_id) REFERENCES symbols(id)
                );",
            )
            .expect("failed to create tables");

            // Insert test symbols
            conn.execute(
                "INSERT INTO symbols (id, file_path, symbol_name, symbol_kind, language, start_line, end_line, scope, signature, file_hash, indexed_at)
                 VALUES (1, '/test/main.rs', 'foo', 'function', 'rust', 10, 20, NULL, 'fn foo()', 'hash1', 0)",
                [],
            )
            .expect("failed to insert symbol 1");

            conn.execute(
                "INSERT INTO symbols (id, file_path, symbol_name, symbol_kind, language, start_line, end_line, scope, signature, file_hash, indexed_at)
                 VALUES (2, '/test/main.rs', 'bar', 'function', 'rust', 30, 40, NULL, 'fn bar()', 'hash1', 0)",
                [],
            )
            .expect("failed to insert symbol 2");

            // Insert test edge: foo calls bar
            conn.execute(
                "INSERT INTO structural_edges (source_symbol_id, target_symbol_name, target_file_path, edge_kind)
                 VALUES (1, 'bar', '/test/main.rs', 'calls')",
                [],
            )
            .expect("failed to insert edge");

            Ok(())
        })
        .expect("database transaction failed");

        // Create graph and bootstrap
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });

        let stats = bootstrap_structural(&mut graph, &db).expect("bootstrap_structural failed");

        // Verify that nodes and edges were created
        assert!(stats.nodes_created > 0, "should create nodes");
        assert!(stats.edges_created > 0, "should create edges");
        assert_eq!(stats.files_processed, 1, "should process 1 file");
        assert!(graph.node_count() > 0, "graph should have nodes");
    }
}
