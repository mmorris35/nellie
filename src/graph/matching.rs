//! Fuzzy entity matching for natural language strings.
//!
//! Resolution order:
//! 1. Normalize input (lowercase, trim, strip common prefixes)
//! 2. Exact match on label_normalized
//! 3. Substring match (input contained in existing label or vice versa)
//! 4. Levenshtein distance (threshold: 2 edits)
//! 5. No match → return None (caller creates new node)

use super::entities::{Entity, EntityType};
use super::memory::GraphMemory;
use strsim::levenshtein;

/// Maximum Levenshtein distance for fuzzy matching.
const MAX_EDIT_DISTANCE: usize = 2;

/// Normalize a label for matching: lowercase, trim, strip common prefixes.
pub fn normalize_label(input: &str) -> String {
    let s = input.trim().to_lowercase();
    // Strip common prefixes like "the ", "a "
    let s = s.strip_prefix("the ").unwrap_or(&s);
    let s = s.strip_prefix("a ").unwrap_or(s);
    s.to_string()
}

/// Match result with confidence score.
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub entity_id: String,
    pub label: String,
    pub match_type: MatchType,
    pub score: f32, // 1.0 = exact, lower = fuzzier
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    Exact,
    Substring,
    Levenshtein,
}

/// Find the best matching entity for the given input string.
/// Returns None if no match found within thresholds.
pub fn find_best_match(
    graph: &GraphMemory,
    input: &str,
    entity_type_hint: Option<EntityType>,
) -> Option<MatchResult> {
    let normalized = normalize_label(input);
    if normalized.is_empty() {
        return None;
    }

    // 1. Exact match
    let exact_matches = graph.entities_by_label(&normalized);
    if let Some(entity) = filter_by_type(exact_matches, entity_type_hint).first() {
        return Some(MatchResult {
            entity_id: entity.id.clone(),
            label: entity.label.clone(),
            match_type: MatchType::Exact,
            score: 1.0,
        });
    }

    // 2. Substring match
    let mut best_substring: Option<MatchResult> = None;
    for entity in all_candidates(graph, entity_type_hint) {
        if entity.label_normalized.contains(&normalized)
            || normalized.contains(&entity.label_normalized)
        {
            let score = normalized.len().min(entity.label_normalized.len()) as f32
                / normalized.len().max(entity.label_normalized.len()) as f32;
            if best_substring.as_ref().map_or(true, |b| score > b.score) {
                best_substring = Some(MatchResult {
                    entity_id: entity.id.clone(),
                    label: entity.label.clone(),
                    match_type: MatchType::Substring,
                    score,
                });
            }
        }
    }
    if let Some(m) = best_substring {
        if m.score > 0.5 {
            return Some(m);
        }
    }

    // 3. Levenshtein distance
    let mut best_lev: Option<(MatchResult, usize)> = None;
    for entity in all_candidates(graph, entity_type_hint) {
        let dist = levenshtein(&normalized, &entity.label_normalized);
        if dist <= MAX_EDIT_DISTANCE && best_lev.as_ref().map_or(true, |(_, d)| dist < *d) {
            best_lev = Some((
                MatchResult {
                    entity_id: entity.id.clone(),
                    label: entity.label.clone(),
                    match_type: MatchType::Levenshtein,
                    score: 1.0 - (dist as f32 / normalized.len().max(1) as f32),
                },
                dist,
            ));
        }
    }
    best_lev.map(|(m, _)| m)
}

fn filter_by_type(entities: Vec<&Entity>, type_hint: Option<EntityType>) -> Vec<&Entity> {
    match type_hint {
        Some(t) => entities
            .into_iter()
            .filter(|e| e.entity_type == t)
            .collect(),
        None => entities,
    }
}

fn all_candidates(graph: &GraphMemory, type_hint: Option<EntityType>) -> Vec<&Entity> {
    type_hint.map_or_else(|| graph.all_entities(), |t| graph.entities_by_type(t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GraphConfig;
    use crate::graph::entities::*;

    fn setup_graph() -> GraphMemory {
        let mut graph = GraphMemory::new(GraphConfig {
            enabled: true,
            ..Default::default()
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        // Add test entities
        for (id, etype, label) in [
            ("n1", EntityType::Concept, "OAuth"),
            ("n2", EntityType::Concept, "oauth-token-refresh"),
            ("n3", EntityType::Tool, "reqwest"),
            ("n4", EntityType::Person, "Alice"),
            ("n5", EntityType::Problem, "rate-limiting"),
        ] {
            graph.add_entity(Entity {
                id: id.to_string(),
                entity_type: etype,
                label: label.to_string(),
                label_normalized: normalize_label(label),
                record_id: None,
                metadata: None,
                created_at: now,
                last_accessed: now,
                access_count: 0,
            });
        }
        graph
    }

    #[test]
    fn test_normalize_label() {
        assert_eq!(normalize_label("  OAuth  "), "oauth");
        assert_eq!(normalize_label("The Bug"), "bug");
        assert_eq!(normalize_label("a problem"), "problem");
    }

    #[test]
    fn test_exact_match() {
        let graph = setup_graph();
        let result = find_best_match(&graph, "oauth", None).unwrap();
        assert_eq!(result.match_type, MatchType::Exact);
        assert_eq!(result.entity_id, "n1");
    }

    #[test]
    fn test_exact_match_case_insensitive() {
        let graph = setup_graph();
        let result = find_best_match(&graph, "OAuth", None).unwrap();
        assert_eq!(result.match_type, MatchType::Exact);
    }

    #[test]
    fn test_substring_match() {
        let graph = setup_graph();
        let result = find_best_match(&graph, "token-refresh", None).unwrap();
        assert_eq!(result.match_type, MatchType::Substring);
        assert_eq!(result.entity_id, "n2");
    }

    #[test]
    fn test_levenshtein_match() {
        let graph = setup_graph();
        let result = find_best_match(&graph, "reqest", None).unwrap(); // typo
        assert_eq!(result.match_type, MatchType::Levenshtein);
        assert_eq!(result.entity_id, "n3");
    }

    #[test]
    fn test_no_match_returns_none() {
        let graph = setup_graph();
        let result = find_best_match(&graph, "completely-unrelated-thing", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_type_hint_filters() {
        let graph = setup_graph();
        // "alice" matches person, not concept
        let result = find_best_match(&graph, "Alice", Some(EntityType::Person)).unwrap();
        assert_eq!(result.entity_id, "n4");
    }

    #[test]
    fn test_empty_input() {
        let graph = setup_graph();
        assert!(find_best_match(&graph, "", None).is_none());
        assert!(find_best_match(&graph, "   ", None).is_none());
    }
}
