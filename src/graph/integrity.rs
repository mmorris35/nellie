//! Graph integrity: confidence decay, garbage collection, outcome processing.

use super::entities::{Outcome, Relationship, RelationshipKind};
use super::memory::GraphMemory;
use crate::config::GraphConfig;

/// Statistics from a decay pass.
#[derive(Debug, Default)]
pub struct DecayStats {
    pub edges_processed: usize,
    pub edges_decayed: usize,
}

/// Statistics from garbage collection.
#[derive(Debug, Default)]
pub struct GcStats {
    pub edges_removed: usize,
    pub nodes_removed: usize,
}

/// Statistics from outcome processing.
#[derive(Debug, Default)]
pub struct OutcomeStats {
    pub reinforced: usize,
    pub weakened: usize,
    pub not_found: usize,
}

/// Run a full decay pass on all edges in the graph.
/// Called at boot and optionally on a daily tick.
///
/// For each edge, computes days since `last_confirmed` and applies
/// exponential decay: `confidence *= 0.5^(days / half_life)`.
pub fn run_decay_pass(graph: &mut GraphMemory, half_life_days: f32) -> DecayStats {
    let mut stats = DecayStats::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    #[allow(clippy::cast_possible_wrap)]
    let now_secs = now as i64;

    // Collect all edge IDs first to avoid borrow issues
    let edge_ids: Vec<String> = graph
        .all_relationships()
        .iter()
        .map(|(_, _, rel)| rel.id.clone())
        .collect();

    for edge_id in &edge_ids {
        if let Some(rel) = graph.get_relationship_mut(edge_id) {
            stats.edges_processed += 1;
            #[allow(clippy::cast_precision_loss)]
            let days_since = (now_secs - rel.last_confirmed) as f32 / 86400.0;
            if days_since > 0.0 {
                let old_conf = rel.confidence;
                rel.decay(days_since, half_life_days);
                if (old_conf - rel.confidence).abs() > f32::EPSILON {
                    stats.edges_decayed += 1;
                }
            }
        }
    }

    tracing::debug!(
        processed = stats.edges_processed,
        decayed = stats.edges_decayed,
        "Decay pass complete"
    );
    stats
}

/// Garbage collect dead edges and orphaned nodes.
///
/// 1. Remove edges with confidence < `gc_min_confidence`
/// 2. Remove nodes with no remaining edges that were created > `gc_orphan_days` ago
pub fn garbage_collect(graph: &mut GraphMemory, config: &GraphConfig) -> GcStats {
    let mut stats = GcStats::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    #[allow(clippy::cast_possible_wrap)]
    let now_secs = now as i64;
    let orphan_cutoff = now_secs - i64::from(config.gc_orphan_days) * 86400;

    // Step 1: Collect dead edge IDs
    let dead_edges: Vec<String> = graph
        .all_relationships()
        .iter()
        .filter(|(_, _, rel)| rel.is_dead(config.gc_min_confidence))
        .map(|(_, _, rel)| rel.id.clone())
        .collect();

    // Remove dead edges (implementation: graph needs a remove_relationship method)
    for edge_id in &dead_edges {
        graph.remove_relationship(edge_id);
        stats.edges_removed += 1;
    }

    // Step 2: Collect orphaned node IDs (no edges, old enough)
    let orphan_ids: Vec<String> = graph
        .all_entities()
        .iter()
        .filter(|entity| {
            let has_edges = !graph.outgoing_edges(&entity.id).is_empty()
                || !graph.incoming_edges(&entity.id).is_empty();
            !has_edges && entity.created_at < orphan_cutoff
        })
        .map(|e| e.id.clone())
        .collect();

    for node_id in &orphan_ids {
        graph.remove_entity(node_id);
        stats.nodes_removed += 1;
    }

    tracing::debug!(
        edges_removed = stats.edges_removed,
        nodes_removed = stats.nodes_removed,
        "Garbage collection complete"
    );
    stats
}

/// Process outcome feedback from a checkpoint.
/// Reinforces or weakens the specified edges based on the outcome.
pub fn process_outcome(
    graph: &mut GraphMemory,
    edge_ids: &[String],
    outcome: Outcome,
) -> OutcomeStats {
    let mut stats = OutcomeStats::default();
    for edge_id in edge_ids {
        if let Some(rel) = graph.get_relationship_mut(edge_id) {
            match outcome {
                Outcome::Success => {
                    rel.reinforce();
                    stats.reinforced += 1;
                }
                Outcome::Failure => {
                    rel.weaken();
                    stats.weakened += 1;
                }
                Outcome::Partial => {
                    rel.reinforce_partial();
                    stats.reinforced += 1;
                }
            }
        } else {
            stats.not_found += 1;
        }
    }
    stats
}

/// Create a `failed_for` contradiction edge when a solution fails for a problem.
/// Returns the edge ID if both entities exist, None otherwise.
pub fn create_contradiction_edge(
    graph: &mut GraphMemory,
    solution_id: &str,
    problem_id: &str,
    context: Option<String>,
) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let suffix_len = 8.min(solution_id.len());
    let edge_id = format!("edge_{}_{}", now, &solution_id[..suffix_len]);
    let rel = Relationship::new_provisional(edge_id.clone(), RelationshipKind::FailedFor, context);
    graph
        .add_relationship(solution_id, problem_id, rel)
        .map(|_| edge_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GraphConfig;
    use crate::graph::entities::*;

    fn test_config() -> GraphConfig {
        GraphConfig {
            enabled: true,
            gc_min_confidence: 0.05,
            gc_orphan_days: 7,
            ..GraphConfig::default()
        }
    }

    fn setup_graph_with_edges() -> GraphMemory {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(Entity::new("n1".into(), EntityType::Agent, "claude".into()));
        graph.add_entity(Entity::new("n2".into(), EntityType::Tool, "cargo".into()));
        graph.add_entity(Entity::new("n3".into(), EntityType::Problem, "slow".into()));
        graph.add_relationship(
            "n1",
            "n2",
            Relationship::new_provisional("e1".into(), RelationshipKind::Used, None),
        );
        graph.add_relationship(
            "n2",
            "n3",
            Relationship::new_provisional("e2".into(), RelationshipKind::Solved, None),
        );
        graph
    }

    #[test]
    fn test_process_outcome_success() {
        let mut graph = setup_graph_with_edges();
        let stats = process_outcome(&mut graph, &["e1".into()], Outcome::Success);
        assert_eq!(stats.reinforced, 1);
        assert_eq!(stats.weakened, 0);
        let rel = graph.get_relationship("e1").unwrap();
        assert!((rel.confidence - 0.5).abs() < f32::EPSILON); // 0.3 + 0.2
    }

    #[test]
    fn test_process_outcome_failure() {
        let mut graph = setup_graph_with_edges();
        let stats = process_outcome(&mut graph, &["e1".into()], Outcome::Failure);
        assert_eq!(stats.weakened, 1);
        let rel = graph.get_relationship("e1").unwrap();
        assert!((rel.confidence - 0.15).abs() < f32::EPSILON); // 0.3 - 0.15
    }

    #[test]
    fn test_process_outcome_partial() {
        let mut graph = setup_graph_with_edges();
        let stats = process_outcome(&mut graph, &["e1".into()], Outcome::Partial);
        assert_eq!(stats.reinforced, 1);
        let rel = graph.get_relationship("e1").unwrap();
        assert!((rel.confidence - 0.35).abs() < f32::EPSILON); // 0.3 + 0.05
    }

    #[test]
    fn test_process_outcome_nonexistent_edge() {
        let mut graph = setup_graph_with_edges();
        let stats = process_outcome(&mut graph, &["nonexistent".into()], Outcome::Success);
        assert_eq!(stats.not_found, 1);
        assert_eq!(stats.reinforced, 0);
    }

    #[test]
    fn test_gc_removes_dead_edges() {
        let mut graph = setup_graph_with_edges();
        // Weaken e1 below threshold
        if let Some(rel) = graph.get_relationship_mut("e1") {
            rel.confidence = 0.01; // Below gc_min_confidence of 0.05
        }
        let config = test_config();
        let stats = garbage_collect(&mut graph, &config);
        assert_eq!(stats.edges_removed, 1);
        assert!(graph.get_relationship("e1").is_none());
        // e2 should still exist
        assert!(graph.get_relationship("e2").is_some());
    }

    #[test]
    fn test_gc_removes_orphaned_nodes() {
        let mut graph = GraphMemory::new(test_config());
        let mut orphan = Entity::new("orphan".into(), EntityType::Concept, "abandoned".into());
        // Set created_at to 30 days ago to exceed gc_orphan_days of 7
        orphan.created_at -= 30 * 86400;
        graph.add_entity(orphan);

        let config = test_config();
        let stats = garbage_collect(&mut graph, &config);
        assert_eq!(stats.nodes_removed, 1);
        assert!(graph.get_entity("orphan").is_none());
    }

    #[test]
    fn test_gc_keeps_connected_nodes() {
        let mut graph = setup_graph_with_edges();
        let config = test_config();
        let stats = garbage_collect(&mut graph, &config);
        // All edges are above threshold, all nodes are connected
        assert_eq!(stats.edges_removed, 0);
        assert_eq!(stats.nodes_removed, 0);
    }

    #[test]
    fn test_create_contradiction_edge() {
        let mut graph = setup_graph_with_edges();
        let edge_id = create_contradiction_edge(&mut graph, "n2", "n3", Some("didn't work".into()));
        assert!(edge_id.is_some());
        let eid = edge_id.unwrap();
        let rel = graph.get_relationship(&eid).unwrap();
        assert_eq!(rel.kind, RelationshipKind::FailedFor);
        assert!(rel.provisional);
    }

    #[test]
    fn test_create_contradiction_edge_missing_node() {
        let mut graph = setup_graph_with_edges();
        let result = create_contradiction_edge(&mut graph, "n2", "nonexistent", None);
        assert!(result.is_none());
    }
}
