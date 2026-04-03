//! Graph enrichment helpers for MCP tool handlers.

use super::entities::{Entity, EntityType, Relationship, RelationshipKind};
use super::matching::find_best_match;
use super::memory::GraphMemory;
use super::persistence;
use crate::storage::Database;
use uuid::Uuid;

/// Resolve a label to an existing entity (fuzzy match) or create a new one.
pub fn ensure_entity(graph: &mut GraphMemory, default_type: EntityType, label: &str) -> String {
    if let Some(m) = find_best_match(graph, label, Some(default_type)) {
        if let Some(entity) = graph.get_entity_mut(&m.entity_id) {
            entity.access_count += 1;
        }
        return m.entity_id;
    }
    let id = format!("{}_{}", default_type.as_str(), Uuid::new_v4());
    let entity = Entity::new(id.clone(), default_type, label.to_string());
    graph.add_entity(entity);
    id
}

/// Create a provisional edge if one doesn't already exist between from→to with this kind.
pub fn ensure_edge(
    graph: &mut GraphMemory,
    from_id: &str,
    to_id: &str,
    kind: RelationshipKind,
    context: Option<String>,
) -> Option<String> {
    for (rel, target) in graph.outgoing_edges(from_id) {
        if target.id == to_id && rel.kind == kind {
            return Some(rel.id.clone());
        }
    }
    let edge_id = format!("edge_{}", Uuid::new_v4());
    let rel = Relationship::new_provisional(edge_id.clone(), kind, context);
    graph.add_relationship(from_id, to_id, rel).map(|_| edge_id)
}

/// Persist new entities and edges to `SQLite` (best-effort, logs on error).
pub fn persist_changes(
    db: &Database,
    graph: &GraphMemory,
    entity_ids: &[String],
    edge_ids: &[String],
) {
    let _ = db.with_conn(|conn| {
        for eid in entity_ids {
            if let Some(entity) = graph.get_entity(eid) {
                if let Err(e) = persistence::save_entity(conn, entity) {
                    tracing::warn!(entity_id = %eid, error = %e, "Failed to persist entity");
                }
            }
        }
        for eid in edge_ids {
            for (from_id, to_id, rel) in graph.all_relationships() {
                if rel.id == *eid {
                    if let Err(e) = persistence::save_relationship(conn, from_id, to_id, rel) {
                        tracing::warn!(edge_id = %eid, error = %e, "Failed to persist edge");
                    }
                    break;
                }
            }
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GraphConfig;

    #[test]
    fn test_ensure_entity_creates_new() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        let id = ensure_entity(&mut graph, EntityType::Tool, "reqwest");
        assert!(graph.get_entity(&id).is_some());
        assert_eq!(graph.node_count(), 1);
    }

    #[test]
    fn test_ensure_entity_reuses_existing() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        graph.add_entity(Entity::new(
            "existing".into(),
            EntityType::Tool,
            "reqwest".into(),
        ));
        let id = ensure_entity(&mut graph, EntityType::Tool, "reqwest");
        assert_eq!(id, "existing");
        assert_eq!(graph.node_count(), 1);
    }

    #[test]
    fn test_ensure_edge_deduplicates() {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        graph.add_entity(Entity::new("n1".into(), EntityType::Agent, "claude".into()));
        graph.add_entity(Entity::new("n2".into(), EntityType::Tool, "cargo".into()));
        let id1 = ensure_edge(&mut graph, "n1", "n2", RelationshipKind::Used, None);
        let id2 = ensure_edge(&mut graph, "n1", "n2", RelationshipKind::Used, None);
        assert_eq!(id1, id2);
        assert_eq!(graph.edge_count(), 1);
    }
}
