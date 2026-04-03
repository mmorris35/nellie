//! Builder pattern query API for graph traversal.

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

use super::entities::{EntityType, RelationshipKind};
use super::memory::GraphMemory;

/// Traversal direction for graph queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    Incoming,
    Both,
}

impl Direction {
    /// Parse from string (for MCP parameter).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "outgoing" | "out" => Self::Outgoing,
            "incoming" | "in" => Self::Incoming,
            _ => Self::Both,
        }
    }
}

/// Summary of an entity in query results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySummary {
    pub id: String,
    pub entity_type: String,
    pub label: String,
}

/// Summary of an edge traversed during a query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSummary {
    pub edge_id: String,
    pub relationship: String,
    pub confidence: f32,
    pub provisional: bool,
}

/// A single result from a graph query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub entity: EntitySummary,
    pub path: Vec<EdgeSummary>,
    pub depth: usize,
}

/// Builder for constructing graph queries.
pub struct GraphQuery<'a> {
    graph: &'a GraphMemory,
    entity_type: Option<EntityType>,
    label_match: Option<String>,
    relationship_filter: Option<RelationshipKind>,
    direction: Direction,
    min_confidence: f32,
    depth: usize,
    limit: usize,
}

impl<'a> GraphQuery<'a> {
    /// Create a new query builder targeting the given graph.
    #[must_use]
    pub fn new(graph: &'a GraphMemory) -> Self {
        Self {
            graph,
            entity_type: None,
            label_match: None,
            relationship_filter: None,
            direction: Direction::Both,
            min_confidence: 0.3,
            depth: 1,
            limit: 10,
        }
    }

    /// Filter results to only entities of this type.
    #[must_use]
    pub fn entity_type(mut self, t: EntityType) -> Self {
        self.entity_type = Some(t);
        self
    }

    /// Fuzzy-match entities by label.
    #[must_use]
    pub fn label(mut self, label: &str) -> Self {
        self.label_match = Some(label.to_string());
        self
    }

    /// Only traverse edges of this relationship kind.
    #[must_use]
    pub fn relationship(mut self, r: RelationshipKind) -> Self {
        self.relationship_filter = Some(r);
        self
    }

    /// Set traversal direction.
    #[must_use]
    pub fn direction(mut self, d: Direction) -> Self {
        self.direction = d;
        self
    }

    /// Minimum confidence threshold for edge traversal.
    #[must_use]
    pub fn min_confidence(mut self, c: f32) -> Self {
        self.min_confidence = c;
        self
    }

    /// Maximum traversal depth from starting nodes.
    #[must_use]
    pub fn depth(mut self, d: usize) -> Self {
        self.depth = d;
        self
    }

    /// Maximum number of results to return.
    #[must_use]
    pub fn limit(mut self, l: usize) -> Self {
        self.limit = l;
        self
    }

    /// Execute the query and return results.
    ///
    /// Algorithm:
    /// 1. Find starting nodes (by label and/or `entity_type` filter)
    /// 2. BFS outward up to `depth` hops, filtering by relationship/confidence
    /// 3. Collect and deduplicate results, cap at `limit`
    #[must_use]
    pub fn execute(&self) -> Vec<QueryResult> {
        // Step 1: find starting node(s)
        let start_ids = self.find_start_nodes();
        if start_ids.is_empty() {
            return Vec::new();
        }

        // Step 2: BFS traversal from start nodes
        let mut results = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        // Mark start nodes as visited (we don't return them as results)
        for id in &start_ids {
            visited.insert(id.clone());
        }

        // BFS queue: (node_id, current_depth, path_so_far)
        let mut queue: VecDeque<(String, usize, Vec<EdgeSummary>)> = VecDeque::new();

        // Seed queue with neighbors of start nodes at depth 1
        for start_id in &start_ids {
            let neighbors = self.get_neighbors(start_id);
            for (neighbor_id, edge_summary) in neighbors {
                if !visited.contains(&neighbor_id) {
                    queue.push_back((neighbor_id, 1, vec![edge_summary]));
                }
            }
        }

        // Process BFS
        while let Some((node_id, current_depth, path)) = queue.pop_front() {
            if visited.contains(&node_id) {
                continue;
            }
            visited.insert(node_id.clone());

            // Check if this node matches entity_type filter (if set)
            if let Some(entity) = self.graph.get_entity(&node_id) {
                let type_matches = self
                    .entity_type
                    .as_ref()
                    .map_or(true, |t| entity.entity_type == *t);

                if type_matches {
                    results.push(QueryResult {
                        entity: EntitySummary {
                            id: entity.id.clone(),
                            entity_type: entity.entity_type.to_string(),
                            label: entity.label.clone(),
                        },
                        path: path.clone(),
                        depth: current_depth,
                    });

                    if results.len() >= self.limit {
                        break;
                    }
                }
            }

            // Continue BFS if within depth limit
            if current_depth < self.depth {
                let neighbors = self.get_neighbors(&node_id);
                for (neighbor_id, edge_summary) in neighbors {
                    if !visited.contains(&neighbor_id) {
                        let mut new_path = path.clone();
                        new_path.push(edge_summary);
                        queue.push_back((neighbor_id, current_depth + 1, new_path));
                    }
                }
            }
        }

        results
    }

    /// Find starting nodes based on `label_match` filter only.
    /// `entity_type` filter applies only to result nodes, not start nodes.
    #[allow(clippy::option_if_let_else)]
    fn find_start_nodes(&self) -> Vec<String> {
        if let Some(ref label) = self.label_match {
            // Use fuzzy matching to find nodes by label (no type filter on start nodes)
            self.graph.fuzzy_match(label)
        } else if let Some(ref entity_type) = self.entity_type {
            // No label filter, get all nodes of this type as start points
            self.graph.get_entities_by_type(entity_type)
        } else {
            // No filters at all — return empty (we need at least one filter)
            Vec::new()
        }
    }

    /// Get neighbors of a node, filtered by direction, relationship, and confidence.
    fn get_neighbors(&self, node_id: &str) -> Vec<(String, EdgeSummary)> {
        let mut neighbors = Vec::new();

        let edges = match self.direction {
            Direction::Outgoing => self.graph.edges_from(node_id),
            Direction::Incoming => self.graph.edges_to(node_id),
            Direction::Both => {
                let mut all = self.graph.edges_from(node_id);
                all.extend(self.graph.edges_to(node_id));
                all
            }
        };

        for (neighbor_id, edge) in edges {
            // Apply confidence filter
            if edge.confidence < self.min_confidence {
                continue;
            }
            // Apply relationship filter
            if let Some(ref rel_filter) = self.relationship_filter {
                if edge.kind != *rel_filter {
                    continue;
                }
            }

            neighbors.push((
                neighbor_id,
                EdgeSummary {
                    edge_id: edge.id.clone(),
                    relationship: edge.kind.to_string(),
                    confidence: edge.confidence,
                    provisional: edge.provisional,
                },
            ));
        }

        neighbors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GraphConfig;
    use crate::graph::entities::{Entity, Relationship};

    fn make_test_graph() -> GraphMemory {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });

        // Create a small graph:
        //   [Agent:claude] --used--> [Tool:cargo] --related_to--> [Concept:rust]
        //   [Agent:claude] --solved--> [Problem:build_error]
        graph.add_entity(Entity::new("a1".into(), EntityType::Agent, "claude".into()));
        graph.add_entity(Entity::new("t1".into(), EntityType::Tool, "cargo".into()));
        graph.add_entity(Entity::new("c1".into(), EntityType::Concept, "rust".into()));
        graph.add_entity(Entity::new(
            "p1".into(),
            EntityType::Problem,
            "build_error".into(),
        ));

        // Create relationships with high confidence
        let mut rel_e1 = Relationship::new_provisional("e1".into(), RelationshipKind::Used, None);
        rel_e1.confidence = 0.8;
        graph.add_relationship("a1", "t1", rel_e1);

        let mut rel_e2 =
            Relationship::new_provisional("e2".into(), RelationshipKind::RelatedTo, None);
        rel_e2.confidence = 0.6;
        graph.add_relationship("t1", "c1", rel_e2);

        let mut rel_e3 = Relationship::new_provisional("e3".into(), RelationshipKind::Solved, None);
        rel_e3.confidence = 0.9;
        graph.add_relationship("a1", "p1", rel_e3);

        // Low-confidence edge that should be filtered
        let mut rel_e4 =
            Relationship::new_provisional("e4".into(), RelationshipKind::RelatedTo, None);
        rel_e4.confidence = 0.1;
        graph.add_relationship("c1", "p1", rel_e4);

        graph
    }

    #[test]
    fn test_query_by_label() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("claude")
            .direction(Direction::Outgoing)
            .depth(1)
            .execute();
        // Should find cargo and build_error (direct outgoing from claude)
        assert_eq!(results.len(), 2);
        let labels: Vec<&str> = results.iter().map(|r| r.entity.label.as_str()).collect();
        assert!(labels.contains(&"cargo"));
        assert!(labels.contains(&"build_error"));
    }

    #[test]
    fn test_query_depth_2() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("claude")
            .direction(Direction::Outgoing)
            .depth(2)
            .execute();
        // Depth 2: claude -> cargo -> rust (via e2), plus claude -> cargo, claude -> build_error
        let labels: Vec<&str> = results.iter().map(|r| r.entity.label.as_str()).collect();
        assert!(
            labels.contains(&"rust"),
            "depth 2 should reach rust via cargo"
        );
    }

    #[test]
    fn test_query_entity_type_filter() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("claude")
            .direction(Direction::Outgoing)
            .entity_type(EntityType::Tool)
            .depth(1)
            .execute();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity.label, "cargo");
    }

    #[test]
    fn test_query_relationship_filter() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("claude")
            .direction(Direction::Outgoing)
            .relationship(RelationshipKind::Used)
            .depth(1)
            .execute();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity.label, "cargo");
    }

    #[test]
    fn test_query_confidence_filter() {
        let graph = make_test_graph();
        // Query from rust concept outgoing — e4 to build_error has confidence 0.1
        let results = GraphQuery::new(&graph)
            .label("rust")
            .direction(Direction::Outgoing)
            .min_confidence(0.3)
            .depth(1)
            .execute();
        // e4 (confidence 0.1) should be filtered out
        assert!(results.is_empty(), "low confidence edge should be filtered");

        // With lower threshold, should find it
        let results = GraphQuery::new(&graph)
            .label("rust")
            .direction(Direction::Outgoing)
            .min_confidence(0.05)
            .depth(1)
            .execute();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity.label, "build_error");
    }

    #[test]
    fn test_query_limit() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("claude")
            .direction(Direction::Outgoing)
            .depth(2)
            .limit(1)
            .execute();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_query_incoming() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("cargo")
            .direction(Direction::Incoming)
            .depth(1)
            .execute();
        // cargo has incoming edge from claude
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity.label, "claude");
    }

    #[test]
    fn test_query_no_filters_returns_empty() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph).execute();
        assert!(
            results.is_empty(),
            "query with no label or type returns empty"
        );
    }

    #[test]
    fn test_query_nonexistent_label() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("nonexistent_thing")
            .depth(1)
            .execute();
        assert!(results.is_empty());
    }

    #[test]
    fn test_query_result_has_path() {
        let graph = make_test_graph();
        let results = GraphQuery::new(&graph)
            .label("claude")
            .direction(Direction::Outgoing)
            .depth(1)
            .execute();
        for result in &results {
            assert_eq!(result.depth, 1);
            assert_eq!(
                result.path.len(),
                1,
                "depth-1 results should have 1 edge in path"
            );
        }
    }
}
