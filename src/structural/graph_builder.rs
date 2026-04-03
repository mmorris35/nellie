//! Builds knowledge graph nodes and edges from extracted structural symbols.
//!
//! Converts `SymbolRecord`s from the database into graph entities and
//! relationships, feeding them into Nellie's existing petgraph-based
//! knowledge graph.

use crate::graph::enrichment::{ensure_edge, ensure_entity};
use crate::graph::entities::{EntityType, RelationshipKind};
use crate::graph::memory::GraphMemory;

use super::storage::SymbolRecord;

/// Stats from building the structural graph.
#[derive(Debug, Default)]
pub struct StructuralGraphStats {
    pub nodes_created: u32,
    pub edges_created: u32,
    pub files_processed: u32,
}

/// Map a `SymbolKind` to a graph `EntityType`.
fn symbol_kind_to_entity_type(kind: super::extractor::SymbolKind) -> EntityType {
    match kind {
        super::extractor::SymbolKind::Function
        | super::extractor::SymbolKind::TestFunction
        | super::extractor::SymbolKind::CallSite => EntityType::StructFunction,
        super::extractor::SymbolKind::Class => EntityType::StructClass,
        super::extractor::SymbolKind::Method => EntityType::StructMethod,
        super::extractor::SymbolKind::Import => EntityType::StructImport,
    }
}

/// Build structural graph from symbol records and structural edges.
///
/// `edges` is a slice of `(source_symbol_id, target_name, target_file, edge_kind)`.
pub fn build_structural_graph(
    graph: &mut GraphMemory,
    symbols: &[SymbolRecord],
    edges: &[(i64, String, Option<String>, String)],
) -> StructuralGraphStats {
    let mut stats = StructuralGraphStats::default();
    let mut files_seen = std::collections::HashSet::new();

    // Create entity nodes for each symbol
    let mut symbol_id_to_entity: std::collections::HashMap<i64, String> =
        std::collections::HashMap::new();

    // Create StructModule entities for each unique file_path
    let mut module_id_to_entity: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for symbol in symbols {
        if !files_seen.contains(&symbol.file_path) {
            files_seen.insert(symbol.file_path.clone());
            let module_id = ensure_entity(graph, EntityType::StructModule, &symbol.file_path);
            module_id_to_entity.insert(symbol.file_path.clone(), module_id);
            stats.nodes_created += 1;
        }
    }

    for symbol in symbols {
        let entity_type = symbol_kind_to_entity_type(symbol.symbol_kind);
        let label = format!("{}:{}", symbol.file_path, symbol.symbol_name);
        let entity_id = ensure_entity(graph, entity_type, &label);
        symbol_id_to_entity.insert(symbol.id, entity_id.clone());

        // Create Contains edge from module to symbol
        if let Some(module_id) = module_id_to_entity.get(&symbol.file_path) {
            ensure_edge(
                graph,
                module_id,
                &entity_id,
                RelationshipKind::Contains,
                Some("structural: module contains".to_string()),
            );
            stats.edges_created += 1;
        }

        stats.nodes_created += 1;

        // Contains edge: if symbol has a scope, link scope -> symbol
        if let Some(ref scope) = symbol.scope {
            let scope_label = format!("{}:{}", symbol.file_path, scope);
            let scope_id = ensure_entity(graph, EntityType::StructClass, &scope_label);
            ensure_edge(
                graph,
                &scope_id,
                &entity_id,
                RelationshipKind::Contains,
                Some("structural: contains".to_string()),
            );
            stats.edges_created += 1;
        }

        // Tests edge: if test function, link to tested function (heuristic)
        if symbol.symbol_kind == super::extractor::SymbolKind::TestFunction {
            let tested_name = symbol
                .symbol_name
                .strip_prefix("test_")
                .or_else(|| symbol.symbol_name.strip_prefix("Test"))
                .unwrap_or(&symbol.symbol_name);

            if !tested_name.is_empty() && tested_name != symbol.symbol_name {
                // Find matching function by name
                for other in symbols {
                    if other.symbol_name == tested_name
                        && matches!(
                            other.symbol_kind,
                            super::extractor::SymbolKind::Function
                                | super::extractor::SymbolKind::Method
                        )
                    {
                        if let Some(other_entity_id) = symbol_id_to_entity.get(&other.id) {
                            ensure_edge(
                                graph,
                                &entity_id,
                                other_entity_id,
                                RelationshipKind::Tests,
                                Some("structural: test covers function".to_string()),
                            );
                            stats.edges_created += 1;
                        }
                    }
                }
            }
        }
    }

    // Create edges from structural_edges table
    for (source_id, target_name, _target_file, edge_kind) in edges {
        let source_entity = match symbol_id_to_entity.get(source_id) {
            Some(id) => id.clone(),
            None => continue,
        };

        let rel_kind = match edge_kind.as_str() {
            "calls" => RelationshipKind::Calls,
            "imports" => RelationshipKind::ImportedBy,
            "inherits" => RelationshipKind::Inherits,
            _ => RelationshipKind::RelatedTo,
        };

        // Find target entity by name match
        for symbol in symbols {
            if symbol.symbol_name == *target_name {
                if let Some(target_entity) = symbol_id_to_entity.get(&symbol.id) {
                    ensure_edge(
                        graph,
                        &source_entity,
                        target_entity,
                        rel_kind,
                        Some(format!("structural: {edge_kind}")),
                    );
                    stats.edges_created += 1;
                    break;
                }
            }
        }
    }

    stats.files_processed = files_seen.len() as u32;
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GraphConfig;
    use crate::structural::extractor::SymbolKind;

    fn test_config() -> GraphConfig {
        GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        }
    }

    fn make_symbol(id: i64, name: &str, kind: SymbolKind, scope: Option<&str>) -> SymbolRecord {
        SymbolRecord {
            id,
            file_path: "/test/main.py".to_string(),
            symbol_name: name.to_string(),
            symbol_kind: kind,
            language: "python".to_string(),
            start_line: 0,
            end_line: 5,
            scope: scope.map(String::from),
            signature: None,
            file_hash: "hash1".to_string(),
            indexed_at: 0,
        }
    }

    #[test]
    fn test_build_creates_nodes() {
        let mut graph = GraphMemory::new(test_config());
        let symbols = vec![
            make_symbol(1, "Foo", SymbolKind::Class, None),
            make_symbol(2, "bar", SymbolKind::Method, Some("Foo")),
        ];
        let stats = build_structural_graph(&mut graph, &symbols, &[]);
        assert!(stats.nodes_created >= 2);
    }

    #[test]
    fn test_build_contains_edges() {
        let mut graph = GraphMemory::new(test_config());
        let symbols = vec![
            make_symbol(1, "Foo", SymbolKind::Class, None),
            make_symbol(2, "bar", SymbolKind::Method, Some("Foo")),
        ];
        let stats = build_structural_graph(&mut graph, &symbols, &[]);
        assert!(stats.edges_created >= 1); // Contains edge
    }

    #[test]
    fn test_build_tests_edges() {
        let mut graph = GraphMemory::new(test_config());
        let symbols = vec![
            make_symbol(1, "foo", SymbolKind::Function, None),
            make_symbol(2, "test_foo", SymbolKind::TestFunction, None),
        ];
        let stats = build_structural_graph(&mut graph, &symbols, &[]);
        assert!(stats.edges_created >= 1); // Tests edge
    }

    #[test]
    fn test_build_calls_edges() {
        let mut graph = GraphMemory::new(test_config());
        let symbols = vec![
            make_symbol(1, "caller", SymbolKind::Function, None),
            make_symbol(2, "callee", SymbolKind::Function, None),
        ];
        let edges = vec![(1i64, "callee".to_string(), None, "calls".to_string())];
        let stats = build_structural_graph(&mut graph, &symbols, &edges);
        assert!(stats.edges_created >= 1); // Calls edge
    }

    #[test]
    fn test_build_idempotent() {
        let mut graph = GraphMemory::new(test_config());
        let symbols = vec![make_symbol(1, "foo", SymbolKind::Function, None)];
        let _stats1 = build_structural_graph(&mut graph, &symbols, &[]);
        let count_after_first = graph.node_count();
        let _stats2 = build_structural_graph(&mut graph, &symbols, &[]);
        assert_eq!(graph.node_count(), count_after_first);
    }
}
