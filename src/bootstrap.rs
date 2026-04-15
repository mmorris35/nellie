//! Bootstrap: import starter lessons into the Nellie database.
//!
//! Embeds lesson files at compile time via `include_str!` and provides
//! functions to parse YAML frontmatter and insert lessons idempotently.

use crate::storage::{
    delete_lesson, insert_lesson, search_lessons_by_text, store_lesson_embedding, Database,
    LessonRecord,
};
use std::path::Path;

/// Embedded bootstrap lesson files, included at compile time.
const BOOTSTRAP_LESSONS: &[&str] = &[
    include_str!("../bootstrap/lessons/01-use-search-hybrid.md"),
    include_str!("../bootstrap/lessons/02-save-checkpoints-with-graph.md"),
    include_str!("../bootstrap/lessons/03-inject-auto-surfaces.md"),
    include_str!("../bootstrap/lessons/04-session-protocol.md"),
    include_str!("../bootstrap/lessons/05-severity-matters.md"),
    include_str!("../bootstrap/lessons/06-checkpoint-resume.md"),
    include_str!("../bootstrap/lessons/07-graph-queries.md"),
    include_str!("../bootstrap/lessons/08-index-your-repos.md"),
];

/// Parsed frontmatter from a bootstrap lesson file.
#[derive(Debug, Clone)]
pub struct BootstrapLesson {
    /// Lesson title.
    pub title: String,
    /// Lesson body content (after the frontmatter).
    pub content: String,
    /// Severity level: "critical", "warning", or "info".
    pub severity: String,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Tools used (graph field).
    pub used_tools: Vec<String>,
    /// Related concepts (graph field).
    pub related_concepts: Vec<String>,
    /// Problem solved (graph field).
    pub solved_problem: Option<String>,
}

/// Result of the bootstrap operation.
#[derive(Debug)]
pub struct BootstrapResult {
    /// Number of lessons successfully imported.
    pub imported: usize,
    /// Number of lessons skipped (already present).
    pub skipped: usize,
}

/// Parse a YAML frontmatter value from a line like `key: "value"` or `key: value`.
fn parse_yaml_string(line: &str) -> String {
    let value = line.trim();
    // Strip surrounding quotes if present
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Parse a YAML array value from a line like `["a", "b", "c"]`.
fn parse_yaml_array(line: &str) -> Vec<String> {
    let value = line.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Vec::new();
    }
    let inner = &value[1..value.len() - 1];
    inner
        .split(',')
        .map(|s| {
            let trimmed = s.trim();
            if (trimmed.starts_with('"') && trimmed.ends_with('"'))
                || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
            {
                trimmed[1..trimmed.len() - 1].to_string()
            } else {
                trimmed.to_string()
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a bootstrap lesson from its raw markdown content.
///
/// Expects YAML frontmatter between `---` delimiters, followed by the body.
///
/// # Errors
///
/// Returns an error if the frontmatter is missing or malformed.
pub fn parse_lesson(raw: &str) -> crate::Result<BootstrapLesson> {
    let mut lines = raw.lines();

    // First line must be `---`
    let first = lines.next().unwrap_or("");
    if first.trim() != "---" {
        return Err(crate::Error::internal(
            "bootstrap lesson missing opening --- frontmatter delimiter",
        ));
    }

    // Collect frontmatter lines until closing `---`
    let mut frontmatter_lines = Vec::new();
    let mut found_closing = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            found_closing = true;
            break;
        }
        frontmatter_lines.push(line);
    }

    if !found_closing {
        return Err(crate::Error::internal(
            "bootstrap lesson missing closing --- frontmatter delimiter",
        ));
    }

    // Remaining lines are the body content
    let body: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();

    // Parse frontmatter key-value pairs
    let mut title = String::new();
    let mut severity = String::from("info");
    let mut tags = Vec::new();
    let mut used_tools = Vec::new();
    let mut related_concepts = Vec::new();
    let mut solved_problem = None;

    for line in &frontmatter_lines {
        if let Some(val) = line.strip_prefix("title:") {
            title = parse_yaml_string(val);
        } else if let Some(val) = line.strip_prefix("severity:") {
            severity = parse_yaml_string(val);
        } else if let Some(val) = line.strip_prefix("tags:") {
            tags = parse_yaml_array(val);
        } else if let Some(val) = line.strip_prefix("used_tools:") {
            used_tools = parse_yaml_array(val);
        } else if let Some(val) = line.strip_prefix("related_concepts:") {
            related_concepts = parse_yaml_array(val);
        } else if let Some(val) = line.strip_prefix("solved_problem:") {
            let parsed = parse_yaml_string(val);
            if !parsed.is_empty() {
                solved_problem = Some(parsed);
            }
        }
    }

    if title.is_empty() {
        return Err(crate::Error::internal(
            "bootstrap lesson has empty or missing title",
        ));
    }

    Ok(BootstrapLesson {
        title,
        content: body,
        severity,
        tags,
        used_tools,
        related_concepts,
        solved_problem,
    })
}

/// Check if a lesson with the given title already exists in the database.
fn lesson_exists_by_title(db: &Database, title: &str) -> crate::Result<bool> {
    let results = db.with_conn(|conn| search_lessons_by_text(conn, title, 50))?;
    // Check for exact title match (text search is LIKE-based, so verify)
    Ok(results.iter().any(|l| l.title == title))
}

/// Run the bootstrap process: parse and import all embedded lessons.
///
/// If `force` is true, deletes existing lessons with matching titles and
/// re-inserts them. Otherwise, skips lessons that already exist.
///
/// When `embedding_service` is provided, generates and stores embeddings
/// for each imported lesson.
///
/// # Errors
///
/// Returns an error if database operations or embedding generation fails.
pub async fn run_bootstrap(
    db: &Database,
    data_dir: &Path,
    force: bool,
) -> crate::Result<BootstrapResult> {
    use crate::embeddings::{EmbeddingConfig, EmbeddingService};

    let mut imported = 0usize;
    let mut skipped = 0usize;

    // Parse all lessons first to fail fast on malformed files
    let mut parsed_lessons = Vec::new();
    for raw in BOOTSTRAP_LESSONS {
        let lesson = parse_lesson(raw)?;
        parsed_lessons.push(lesson);
    }

    // Try to initialize embedding service (non-fatal if unavailable)
    let embedding_service = {
        let emb_config = EmbeddingConfig::from_data_dir(data_dir, 2);
        if emb_config.model_path.exists() && emb_config.tokenizer_path.exists() {
            let svc = EmbeddingService::new(emb_config);
            match svc.init().await {
                Ok(()) => {
                    tracing::info!("Embedding service initialized for bootstrap");
                    Some(svc)
                }
                Err(e) => {
                    tracing::warn!(
                        "Embedding service unavailable, lessons will not have \
                         embeddings: {e}"
                    );
                    None
                }
            }
        } else {
            tracing::warn!(
                "Embedding model not found, lessons will not have embeddings. \
                 Run `nellie setup` to download the model."
            );
            None
        }
    };

    for parsed in &parsed_lessons {
        let exists = lesson_exists_by_title(db, &parsed.title)?;

        if exists && !force {
            tracing::debug!(title = %parsed.title, "Lesson already exists, skipping");
            skipped += 1;
            continue;
        }

        // If force mode and lesson exists, delete the old one
        if exists && force {
            let old_results =
                db.with_conn(|conn| search_lessons_by_text(conn, &parsed.title, 50))?;
            for old in &old_results {
                if old.title == parsed.title {
                    db.with_conn(|conn| delete_lesson(conn, &old.id))?;
                    tracing::debug!(
                        title = %parsed.title,
                        "Deleted existing lesson for re-import"
                    );
                }
            }
        }

        // Build the LessonRecord
        let record = LessonRecord::new(&parsed.title, &parsed.content, parsed.tags.clone())
            .with_severity(&parsed.severity)
            .with_agent("nellie-bootstrap");

        let lesson_id = record.id.clone();

        // Insert the lesson
        db.with_conn(|conn| insert_lesson(conn, &record))?;

        // Generate and store embedding if service is available
        if let Some(ref svc) = embedding_service {
            let embed_text = format!("{} {}", parsed.title, parsed.content);
            match svc.embed_one(embed_text).await {
                Ok(embedding) => {
                    if let Err(e) =
                        db.with_conn(|conn| store_lesson_embedding(conn, &lesson_id, &embedding))
                    {
                        tracing::warn!(
                            title = %parsed.title,
                            "Failed to store embedding: {e}"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        title = %parsed.title,
                        "Failed to generate embedding: {e}"
                    );
                }
            }
        }

        tracing::info!(title = %parsed.title, severity = %parsed.severity, "Imported lesson");
        imported += 1;
    }

    Ok(BootstrapResult { imported, skipped })
}

/// Enrich the knowledge graph with bootstrap lesson metadata.
///
/// Uses `used_tools`, `related_concepts`, and `solved_problem` fields
/// from each parsed lesson to create graph entities and edges.
pub fn enrich_graph_from_bootstrap(
    graph: &mut crate::graph::memory::GraphMemory,
    lessons: &[BootstrapLesson],
) -> (u32, u32) {
    use crate::graph::enrichment::{ensure_edge, ensure_entity};
    use crate::graph::entities::{EntityType, RelationshipKind};

    let mut total_nodes = 0u32;
    let mut total_edges = 0u32;

    for lesson in lessons {
        // Create solution node for the lesson
        let solution_id = ensure_entity(graph, EntityType::Solution, &lesson.title);
        total_nodes += 1;

        // Create tool nodes and edges
        for tool in &lesson.used_tools {
            let tool_id = ensure_entity(graph, EntityType::Tool, tool);
            ensure_edge(
                graph,
                &solution_id,
                &tool_id,
                RelationshipKind::Used,
                Some("Bootstrap: lesson used_tools".to_string()),
            );
            total_nodes += 1;
            total_edges += 1;
        }

        // Create concept nodes and edges
        for concept in &lesson.related_concepts {
            let concept_id = ensure_entity(graph, EntityType::Concept, concept);
            ensure_edge(
                graph,
                &solution_id,
                &concept_id,
                RelationshipKind::RelatedTo,
                Some("Bootstrap: lesson related_concepts".to_string()),
            );
            total_nodes += 1;
            total_edges += 1;
        }

        // Create problem node if solved_problem is specified
        if let Some(ref problem) = lesson.solved_problem {
            let problem_id = ensure_entity(graph, EntityType::Problem, problem);
            ensure_edge(
                graph,
                &solution_id,
                &problem_id,
                RelationshipKind::Solved,
                Some("Bootstrap: lesson solved_problem".to_string()),
            );
            total_nodes += 1;
            total_edges += 1;
        }
    }

    (total_nodes, total_edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_yaml_string_with_quotes() {
        assert_eq!(parse_yaml_string(" \"hello world\" "), "hello world");
    }

    #[test]
    fn test_parse_yaml_string_without_quotes() {
        assert_eq!(parse_yaml_string(" critical "), "critical");
    }

    #[test]
    fn test_parse_yaml_array() {
        let result = parse_yaml_array(r#"["a", "b", "c"]"#);
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_yaml_array_empty() {
        let result = parse_yaml_array("[]");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_yaml_array_not_array() {
        let result = parse_yaml_array("not an array");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_lesson_valid() {
        let raw = r#"---
title: "Test lesson"
severity: warning
tags: ["tag1", "tag2"]
used_tools: ["tool1"]
related_concepts: ["concept1"]
solved_problem: "some problem"
---

This is the body content."#;

        let lesson = parse_lesson(raw).expect("should parse");
        assert_eq!(lesson.title, "Test lesson");
        assert_eq!(lesson.severity, "warning");
        assert_eq!(lesson.tags, vec!["tag1", "tag2"]);
        assert_eq!(lesson.used_tools, vec!["tool1"]);
        assert_eq!(lesson.related_concepts, vec!["concept1"]);
        assert_eq!(lesson.solved_problem, Some("some problem".to_string()));
        assert_eq!(lesson.content, "This is the body content.");
    }

    #[test]
    fn test_parse_lesson_missing_frontmatter() {
        let raw = "No frontmatter here";
        assert!(parse_lesson(raw).is_err());
    }

    #[test]
    fn test_parse_lesson_missing_closing() {
        let raw = "---\ntitle: \"test\"\nno closing";
        assert!(parse_lesson(raw).is_err());
    }

    #[test]
    fn test_parse_lesson_missing_title() {
        let raw = "---\nseverity: info\n---\nBody";
        assert!(parse_lesson(raw).is_err());
    }

    #[test]
    fn test_parse_all_bootstrap_lessons() {
        for (i, raw) in BOOTSTRAP_LESSONS.iter().enumerate() {
            let lesson =
                parse_lesson(raw).unwrap_or_else(|e| panic!("lesson {i} failed to parse: {e}"));
            assert!(!lesson.title.is_empty(), "lesson {i} has empty title");
            assert!(!lesson.content.is_empty(), "lesson {i} has empty content");
            assert!(
                ["critical", "warning", "info"].contains(&lesson.severity.as_str()),
                "lesson {i} has invalid severity: {}",
                lesson.severity
            );
        }
    }

    #[test]
    fn test_parse_lesson_severity_values() {
        let mut critical_count = 0;
        let mut warning_count = 0;
        let mut info_count = 0;

        for raw in BOOTSTRAP_LESSONS {
            let lesson = parse_lesson(raw).expect("parse ok");
            match lesson.severity.as_str() {
                "critical" => critical_count += 1,
                "warning" => warning_count += 1,
                "info" => info_count += 1,
                _ => panic!("unexpected severity"),
            }
        }

        assert!(critical_count >= 1, "need at least 1 critical lesson");
        assert!(warning_count >= 1, "need at least 1 warning lesson");
        assert!(info_count >= 1, "need at least 1 info lesson");
    }

    #[test]
    fn test_bootstrap_lesson_count() {
        assert_eq!(
            BOOTSTRAP_LESSONS.len(),
            8,
            "should have exactly 8 bootstrap lessons"
        );
    }

    #[test]
    fn test_enrich_graph_from_bootstrap() {
        use crate::config::GraphConfig;
        use crate::graph::memory::GraphMemory;

        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });

        let lessons: Vec<BootstrapLesson> = BOOTSTRAP_LESSONS
            .iter()
            .map(|raw| parse_lesson(raw).expect("parse ok"))
            .collect();

        let (nodes, edges) = enrich_graph_from_bootstrap(&mut graph, &lessons);
        assert!(nodes > 0, "should create nodes");
        assert!(edges > 0, "should create edges");
        assert!(
            graph.node_count() > 0,
            "graph should have nodes after enrichment"
        );
    }
}
