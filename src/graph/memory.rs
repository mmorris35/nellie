//! In-memory graph backed by petgraph.

use std::collections::HashMap;

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

use crate::config::GraphConfig;

#[allow(unused_imports)]
use super::entities::{Entity, EntityType, Relationship, RelationshipKind};

/// In-memory knowledge graph.
#[derive(Clone)]
pub struct GraphMemory {
    graph: DiGraph<Entity, Relationship>,
    /// Map from entity ID to petgraph NodeIndex
    node_index: HashMap<String, NodeIndex>,
    /// Map from normalized label to list of NodeIndexes (for fuzzy matching)
    label_index: HashMap<String, Vec<NodeIndex>>,
    #[allow(dead_code)]
    config: GraphConfig,
}

impl GraphMemory {
    /// Create a new empty graph.
    #[must_use]
    pub fn new(config: GraphConfig) -> Self {
        Self {
            graph: DiGraph::new(),
            node_index: HashMap::new(),
            label_index: HashMap::new(),
            config,
        }
    }

    /// Add an entity node. Returns the NodeIndex.
    /// If an entity with the same ID already exists, returns its existing index.
    pub fn add_entity(&mut self, entity: Entity) -> NodeIndex {
        let id = entity.id.clone();
        let label_normalized = entity.label_normalized.clone();

        // Check if entity already exists
        if let Some(&idx) = self.node_index.get(&id) {
            return idx;
        }

        // Add to graph
        let idx = self.graph.add_node(entity);

        // Update indices
        self.node_index.insert(id, idx);
        self.label_index
            .entry(label_normalized)
            .or_default()
            .push(idx);

        idx
    }

    /// Get an entity by ID.
    pub fn get_entity(&self, id: &str) -> Option<&Entity> {
        self.node_index
            .get(id)
            .and_then(|&idx| self.graph.node_weight(idx))
    }

    /// Get a mutable entity by ID.
    pub fn get_entity_mut(&mut self, id: &str) -> Option<&mut Entity> {
        let idx = *self.node_index.get(id)?;
        self.graph.node_weight_mut(idx)
    }

    /// Remove an entity and all its edges.
    pub fn remove_entity(&mut self, id: &str) -> Option<Entity> {
        let idx = self.node_index.remove(id)?;
        let entity = self.graph.remove_node(idx)?;

        // Remove from label index
        if let Some(indices) = self.label_index.get_mut(&entity.label_normalized) {
            indices.retain(|&i| i != idx);
        }

        Some(entity)
    }

    /// Add a relationship edge between two entities (by entity ID).
    /// Returns None if either entity doesn't exist.
    pub fn add_relationship(
        &mut self,
        from_id: &str,
        to_id: &str,
        relationship: Relationship,
    ) -> Option<EdgeIndex> {
        let from_idx = *self.node_index.get(from_id)?;
        let to_idx = *self.node_index.get(to_id)?;

        Some(self.graph.add_edge(from_idx, to_idx, relationship))
    }

    /// Get a relationship by edge ID.
    pub fn get_relationship(&self, edge_id: &str) -> Option<&Relationship> {
        self.graph.edge_indices().find_map(|idx| {
            let rel = self.graph.edge_weight(idx)?;
            if rel.id == edge_id {
                Some(rel)
            } else {
                None
            }
        })
    }

    /// Get a mutable relationship by edge ID.
    pub fn get_relationship_mut(&mut self, edge_id: &str) -> Option<&mut Relationship> {
        self.graph
            .edge_indices()
            .find(|&idx| self.graph.edge_weight(idx).is_some_and(|r| r.id == edge_id))
            .and_then(|idx| self.graph.edge_weight_mut(idx))
    }

    /// Remove a relationship by edge ID.
    /// Returns the removed relationship if it existed, None otherwise.
    pub fn remove_relationship(&mut self, edge_id: &str) -> Option<Relationship> {
        let edge_idx = self
            .graph
            .edge_indices()
            .find(|&idx| self.graph.edge_weight(idx).is_some_and(|r| r.id == edge_id))?;
        self.graph.remove_edge(edge_idx)
    }

    /// Find entities by normalized label. Returns a Vec of entity IDs.
    #[must_use]
    pub fn find_by_label(&self, label_normalized: &str) -> Option<Vec<String>> {
        self.label_index.get(label_normalized).map(|indices| {
            indices
                .iter()
                .filter_map(|&idx| self.graph.node_weight(idx).map(|e| e.id.clone()))
                .collect()
        })
    }

    /// Find entities by type.
    pub fn entities_by_type(&self, entity_type: EntityType) -> Vec<&Entity> {
        self.graph
            .node_weights()
            .filter(|e| e.entity_type == entity_type)
            .collect()
    }

    /// Find entities by normalized label (exact match).
    pub fn entities_by_label(&self, label_normalized: &str) -> Vec<&Entity> {
        self.label_index
            .get(label_normalized)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&idx| self.graph.node_weight(idx))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all edges from a node (outgoing).
    pub fn outgoing_edges(&self, entity_id: &str) -> Vec<(&Relationship, &Entity)> {
        let Some(&node_idx) = self.node_index.get(entity_id) else {
            return Vec::new();
        };

        self.graph
            .neighbors(node_idx)
            .filter_map(|target_idx| {
                let edge_idx = self
                    .graph
                    .find_edge(node_idx, target_idx)
                    .or_else(|| self.graph.find_edge(target_idx, node_idx))?;
                let rel = self.graph.edge_weight(edge_idx)?;
                let target_entity = self.graph.node_weight(target_idx)?;
                Some((rel, target_entity))
            })
            .collect()
    }

    /// Get all edges to a node (incoming).
    pub fn incoming_edges(&self, entity_id: &str) -> Vec<(&Relationship, &Entity)> {
        let Some(&node_idx) = self.node_index.get(entity_id) else {
            return Vec::new();
        };

        let mut result = Vec::new();

        // Check all nodes for edges pointing to node_idx
        for source_idx in self.graph.node_indices() {
            if let Some(edge_idx) = self.graph.find_edge(source_idx, node_idx) {
                if let Some(rel) = self.graph.edge_weight(edge_idx) {
                    if let Some(source_entity) = self.graph.node_weight(source_idx) {
                        result.push((rel, source_entity));
                    }
                }
            }
        }

        result
    }

    /// Traverse the graph from a starting entity, returning all entities within depth hops.
    /// Filters by minimum confidence.
    pub fn traverse(
        &self,
        start_id: &str,
        depth: usize,
        min_confidence: f32,
    ) -> Vec<(Vec<&Relationship>, &Entity)> {
        let Some(&start_idx) = self.node_index.get(start_id) else {
            return Vec::new();
        };

        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        queue.push_back((start_idx, 0, Vec::new()));
        visited.insert(start_idx);

        while let Some((current_idx, current_depth, path)) = queue.pop_front() {
            if current_depth >= depth {
                continue;
            }

            // Get neighbors
            for neighbor_idx in self.graph.neighbors(current_idx) {
                if visited.contains(&neighbor_idx) {
                    continue;
                }

                visited.insert(neighbor_idx);

                // Get edge
                if let Some(edge_idx) = self.graph.find_edge(current_idx, neighbor_idx) {
                    if let Some(rel) = self.graph.edge_weight(edge_idx) {
                        if rel.confidence >= min_confidence {
                            let mut new_path = path.clone();
                            new_path.push(rel);

                            if let Some(entity) = self.graph.node_weight(neighbor_idx) {
                                result.push((new_path.clone(), entity));

                                // Queue for further traversal
                                queue.push_back((neighbor_idx, current_depth + 1, new_path));
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// Number of nodes in the graph.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges in the graph.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get all entities (for persistence).
    pub fn all_entities(&self) -> Vec<&Entity> {
        self.graph.node_weights().collect()
    }

    /// Get all relationships with their from/to entity IDs (for persistence).
    pub fn all_relationships(&self) -> Vec<(&str, &str, &Relationship)> {
        self.graph
            .edge_indices()
            .filter_map(|edge_idx| {
                let (from_idx, to_idx) = self.graph.edge_endpoints(edge_idx)?;
                let from_entity = self.graph.node_weight(from_idx)?;
                let to_entity = self.graph.node_weight(to_idx)?;
                let rel = self.graph.edge_weight(edge_idx)?;
                Some((from_entity.id.as_str(), to_entity.id.as_str(), rel))
            })
            .collect()
    }

    /// Get entity IDs by type (query builder helper).
    pub fn get_entities_by_type(&self, entity_type: &EntityType) -> Vec<String> {
        self.graph
            .node_weights()
            .filter(|e| e.entity_type == *entity_type)
            .map(|e| e.id.clone())
            .collect()
    }

    /// Fuzzy match entities by label (query builder helper).
    /// Currently does simple substring matching; can be extended with strsim later.
    pub fn fuzzy_match(&self, label: &str) -> Vec<String> {
        let label_lower = label.to_lowercase();
        self.graph
            .node_weights()
            .filter(|e| e.label.to_lowercase().contains(&label_lower))
            .map(|e| e.id.clone())
            .collect()
    }

    /// Get outgoing edges from a node with neighbor IDs (query builder helper).
    /// Returns (neighbor_id, relationship) tuples.
    pub fn edges_from(&self, node_id: &str) -> Vec<(String, &Relationship)> {
        let Some(&node_idx) = self.node_index.get(node_id) else {
            return Vec::new();
        };

        self.graph
            .neighbors(node_idx)
            .filter_map(|target_idx| {
                let edge_idx = self
                    .graph
                    .find_edge(node_idx, target_idx)
                    .or_else(|| self.graph.find_edge(target_idx, node_idx))?;
                let target_entity = self.graph.node_weight(target_idx)?;
                let rel = self.graph.edge_weight(edge_idx)?;
                Some((target_entity.id.clone(), rel))
            })
            .collect()
    }

    /// Get incoming edges to a node with source IDs (query builder helper).
    /// Returns (source_id, relationship) tuples.
    pub fn edges_to(&self, node_id: &str) -> Vec<(String, &Relationship)> {
        let Some(&node_idx) = self.node_index.get(node_id) else {
            return Vec::new();
        };

        let mut result = Vec::new();

        // Check all nodes for edges pointing to node_idx
        for source_idx in self.graph.node_indices() {
            if let Some(edge_idx) = self.graph.find_edge(source_idx, node_idx) {
                if let Some(rel) = self.graph.edge_weight(edge_idx) {
                    if let Some(source_entity) = self.graph.node_weight(source_idx) {
                        result.push((source_entity.id.clone(), rel));
                    }
                }
            }
        }

        result
    }
    /// Find all entities with a given record_id (used to link to vector store chunks, lessons, etc).
    /// Returns a list of entity IDs that reference this record.
    #[must_use]
    pub fn find_by_record_id(&self, record_id: &str) -> Vec<String> {
        self.graph
            .node_indices()
            .filter_map(|idx| {
                self.graph.node_weight(idx).and_then(|entity| {
                    if entity.record_id.as_deref() == Some(record_id) {
                        Some(entity.id.clone())
                    } else {
                        None
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> GraphConfig {
        GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        }
    }

    fn make_entity(id: &str, etype: EntityType, label: &str) -> Entity {
        Entity::new(id.to_string(), etype, label.to_string())
    }

    fn make_edge(id: &str, kind: RelationshipKind) -> Relationship {
        Relationship::new_provisional(id.to_string(), kind, None)
    }

    #[test]
    fn test_add_and_get_entity() {
        let mut graph = GraphMemory::new(test_config());
        let entity = make_entity("n1", EntityType::Tool, "reqwest");
        graph.add_entity(entity);
        let retrieved = graph.get_entity("n1").unwrap();
        assert_eq!(retrieved.label, "reqwest");
        assert_eq!(retrieved.entity_type, EntityType::Tool);
    }

    #[test]
    fn test_duplicate_entity_returns_existing() {
        let mut graph = GraphMemory::new(test_config());
        let e1 = make_entity("n1", EntityType::Tool, "reqwest");
        let e2 = make_entity("n1", EntityType::Tool, "reqwest-updated");
        let idx1 = graph.add_entity(e1);
        let idx2 = graph.add_entity(e2);
        assert_eq!(idx1, idx2);
        assert_eq!(graph.node_count(), 1);
    }

    #[test]
    fn test_add_relationship() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Tool, "reqwest"));
        graph.add_entity(make_entity("n2", EntityType::Problem, "timeout"));
        let edge = make_edge("e1", RelationshipKind::Solved);
        let result = graph.add_relationship("n1", "n2", edge);
        assert!(result.is_some());
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_relationship_missing_entity_returns_none() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Tool, "reqwest"));
        let edge = make_edge("e1", RelationshipKind::Solved);
        let result = graph.add_relationship("n1", "nonexistent", edge);
        assert!(result.is_none());
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_outgoing_edges() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Agent, "claude"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "reqwest"));
        graph.add_entity(make_entity("n3", EntityType::Tool, "serde"));
        graph.add_relationship("n1", "n2", make_edge("e1", RelationshipKind::Used));
        graph.add_relationship("n1", "n3", make_edge("e2", RelationshipKind::Used));
        let edges = graph.outgoing_edges("n1");
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn test_incoming_edges() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Agent, "claude"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "reqwest"));
        graph.add_relationship("n1", "n2", make_edge("e1", RelationshipKind::Used));
        let edges = graph.incoming_edges("n2");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].1.label, "claude");
    }

    #[test]
    fn test_traverse_depth_one() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Agent, "claude"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "reqwest"));
        graph.add_entity(make_entity("n3", EntityType::Concept, "http"));
        graph.add_relationship("n1", "n2", make_edge("e1", RelationshipKind::Used));
        graph.add_relationship("n2", "n3", make_edge("e2", RelationshipKind::RelatedTo));
        let results = graph.traverse("n1", 1, 0.0);
        // Depth 1: should find n2 but not n3
        let ids: Vec<&str> = results.iter().map(|(_, e)| e.id.as_str()).collect();
        assert!(ids.contains(&"n2"));
        assert!(!ids.contains(&"n3"));
    }

    #[test]
    fn test_traverse_depth_two() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Agent, "claude"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "reqwest"));
        graph.add_entity(make_entity("n3", EntityType::Concept, "http"));
        graph.add_relationship("n1", "n2", make_edge("e1", RelationshipKind::Used));
        graph.add_relationship("n2", "n3", make_edge("e2", RelationshipKind::RelatedTo));
        let results = graph.traverse("n1", 2, 0.0);
        let ids: Vec<&str> = results.iter().map(|(_, e)| e.id.as_str()).collect();
        assert!(ids.contains(&"n2"));
        assert!(ids.contains(&"n3"));
    }

    #[test]
    fn test_traverse_respects_min_confidence() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Agent, "claude"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "reqwest"));
        // Default provisional edge has confidence 0.3
        graph.add_relationship("n1", "n2", make_edge("e1", RelationshipKind::Used));
        // min_confidence 0.5 should exclude the 0.3 edge
        let results = graph.traverse("n1", 1, 0.5);
        assert!(results.is_empty());
        // min_confidence 0.2 should include it
        let results = graph.traverse("n1", 1, 0.2);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_remove_entity_removes_edges() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Agent, "claude"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "reqwest"));
        graph.add_relationship("n1", "n2", make_edge("e1", RelationshipKind::Used));
        assert_eq!(graph.edge_count(), 1);
        graph.remove_entity("n1");
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_entities_by_type() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Tool, "reqwest"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "serde"));
        graph.add_entity(make_entity("n3", EntityType::Person, "mike"));
        let tools = graph.entities_by_type(EntityType::Tool);
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn test_entities_by_label() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Tool, "reqwest"));
        graph.add_entity(make_entity("n2", EntityType::Concept, "oauth"));
        let results = graph.entities_by_label("reqwest");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "n1");
    }

    #[test]
    fn test_empty_graph() {
        let graph = GraphMemory::new(test_config());
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
        assert!(graph.get_entity("nonexistent").is_none());
    }

    #[test]
    fn test_remove_relationship() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Agent, "claude"));
        graph.add_entity(make_entity("n2", EntityType::Tool, "reqwest"));
        graph.add_relationship("n1", "n2", make_edge("e1", RelationshipKind::Used));
        assert_eq!(graph.edge_count(), 1);
        let removed = graph.remove_relationship("e1");
        assert!(removed.is_some());
        assert_eq!(graph.edge_count(), 0);
        assert!(graph.get_relationship("e1").is_none());
    }

    #[test]
    fn test_remove_nonexistent_relationship() {
        let mut graph = GraphMemory::new(test_config());
        let result = graph.remove_relationship("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_by_label() {
        let mut graph = GraphMemory::new(test_config());
        graph.add_entity(make_entity("n1", EntityType::Tool, "reqwest"));
        graph.add_entity(make_entity("n2", EntityType::Concept, "oauth"));
        let results = graph.find_by_label("reqwest");
        assert!(results.is_some());
        assert_eq!(results.unwrap().len(), 1);
        let results = graph.find_by_label("nonexistent");
        assert!(results.is_none());
    }
}
