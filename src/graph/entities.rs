//! Entity and relationship type definitions for the knowledge graph.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Types of entities that can exist as graph nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Agent,
    Tool,
    Problem,
    Solution,
    Concept,
    Person,
    Project,
    Chunk,
    StructFunction,
    StructClass,
    StructMethod,
    StructModule,
    StructImport,
}

impl EntityType {
    /// String representation for SQLite storage.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Tool => "tool",
            Self::Problem => "problem",
            Self::Solution => "solution",
            Self::Concept => "concept",
            Self::Person => "person",
            Self::Project => "project",
            Self::Chunk => "chunk",
            Self::StructFunction => "struct_function",
            Self::StructClass => "struct_class",
            Self::StructMethod => "struct_method",
            Self::StructModule => "struct_module",
            Self::StructImport => "struct_import",
        }
    }

    /// Parse from string (SQLite storage format).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "tool" => Some(Self::Tool),
            "problem" => Some(Self::Problem),
            "solution" => Some(Self::Solution),
            "concept" => Some(Self::Concept),
            "person" => Some(Self::Person),
            "project" => Some(Self::Project),
            "chunk" => Some(Self::Chunk),
            "struct_function" => Some(Self::StructFunction),
            "struct_class" => Some(Self::StructClass),
            "struct_method" => Some(Self::StructMethod),
            "struct_module" => Some(Self::StructModule),
            "struct_import" => Some(Self::StructImport),
            _ => None,
        }
    }
}

impl fmt::Display for EntityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub entity_type: EntityType,
    pub label: String,
    pub label_normalized: String,
    /// Optional FK to chunks.id, lessons.id, or checkpoints.id
    pub record_id: Option<String>,
    /// Extensible JSON metadata
    pub metadata: Option<serde_json::Value>,
    /// Unix timestamp (seconds since epoch)
    pub created_at: i64,
    /// Unix timestamp (seconds since epoch)
    pub last_accessed: i64,
    pub access_count: u32,
}

impl Entity {
    /// Create a new entity with sensible defaults.
    #[must_use]
    pub fn new(id: String, entity_type: EntityType, label: String) -> Self {
        let normalized = label.trim().to_lowercase();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        #[allow(clippy::cast_possible_wrap)]
        let now = now as i64;
        Self {
            id,
            entity_type,
            label,
            label_normalized: normalized,
            record_id: None,
            metadata: None,
            created_at: now,
            last_accessed: now,
            access_count: 0,
        }
    }
}

/// Types of relationships between entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    Used,
    Solved,
    FailedFor,
    Knows,
    Prefers,
    DependsOn,
    RelatedTo,
    DerivedFrom,
    Encountered,
    SolvedBy,
    Calls,
    ImportedBy,
    Inherits,
    Tests,
    Contains,
}

impl RelationshipKind {
    /// String representation for SQLite storage.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Used => "used",
            Self::Solved => "solved",
            Self::FailedFor => "failed_for",
            Self::Knows => "knows",
            Self::Prefers => "prefers",
            Self::DependsOn => "depends_on",
            Self::RelatedTo => "related_to",
            Self::DerivedFrom => "derived_from",
            Self::Encountered => "encountered",
            Self::SolvedBy => "solved_by",
            Self::Calls => "calls",
            Self::ImportedBy => "imported_by",
            Self::Inherits => "inherits",
            Self::Tests => "tests",
            Self::Contains => "contains",
        }
    }

    /// Parse from string (SQLite storage format).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "used" => Some(Self::Used),
            "solved" => Some(Self::Solved),
            "failed_for" => Some(Self::FailedFor),
            "knows" => Some(Self::Knows),
            "prefers" => Some(Self::Prefers),
            "depends_on" => Some(Self::DependsOn),
            "related_to" => Some(Self::RelatedTo),
            "derived_from" => Some(Self::DerivedFrom),
            "encountered" => Some(Self::Encountered),
            "solved_by" => Some(Self::SolvedBy),
            "calls" => Some(Self::Calls),
            "imported_by" => Some(Self::ImportedBy),
            "inherits" => Some(Self::Inherits),
            "tests" => Some(Self::Tests),
            "contains" => Some(Self::Contains),
            _ => None,
        }
    }
}

impl fmt::Display for RelationshipKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An edge in the knowledge graph with confidence tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub id: String,
    pub kind: RelationshipKind,
    pub confidence: f32,
    pub provisional: bool,
    pub context: Option<String>,
    /// Unix timestamp (seconds since epoch)
    pub created_at: i64,
    /// Unix timestamp (seconds since epoch)
    pub last_confirmed: i64,
    pub access_count: u32,
    pub success_count: u32,
    pub failure_count: u32,
}

/// Helper: current Unix timestamp in seconds.
fn now_epoch() -> i64 {
    #[allow(clippy::cast_possible_wrap)]
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    ts
}

impl Relationship {
    /// Default confidence for new provisional edges.
    pub const PROVISIONAL_CONFIDENCE: f32 = 0.3;

    /// Create a new provisional relationship.
    #[must_use]
    pub fn new_provisional(id: String, kind: RelationshipKind, context: Option<String>) -> Self {
        let now = now_epoch();
        Self {
            id,
            kind,
            confidence: Self::PROVISIONAL_CONFIDENCE,
            provisional: true,
            context,
            created_at: now,
            last_confirmed: now,
            access_count: 0,
            success_count: 0,
            failure_count: 0,
        }
    }

    /// Reinforce this edge (positive outcome).
    pub fn reinforce(&mut self) {
        self.confidence = (self.confidence + 0.2).min(1.0);
        self.last_confirmed = now_epoch();
        self.access_count += 1;
        self.success_count += 1;
        if self.provisional && self.success_count >= 2 {
            self.provisional = false;
        }
    }

    /// Weaken this edge (negative outcome).
    pub fn weaken(&mut self) {
        self.confidence = (self.confidence - 0.15).max(0.0);
        self.access_count += 1;
        self.failure_count += 1;
    }

    /// Apply minor reinforcement (partial outcome).
    pub fn reinforce_partial(&mut self) {
        self.confidence = (self.confidence + 0.05).min(1.0);
        self.last_confirmed = now_epoch();
        self.access_count += 1;
    }

    /// Apply confidence decay based on days since last confirmation.
    /// `days_since` is pre-computed by the caller (allows testing without time mocking).
    pub fn decay(&mut self, days_since: f32, half_life_days: f32) {
        if days_since > 0.0 {
            self.confidence *= 0.5_f32.powf(days_since / half_life_days);
        }
    }

    /// Should this edge be garbage collected?
    #[must_use]
    pub fn is_dead(&self, min_confidence: f32) -> bool {
        self.confidence < min_confidence
    }
}

/// Outcome of a session that used graph suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Partial,
}

impl Outcome {
    /// Parse from string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "success" => Some(Self::Success),
            "failure" => Some(Self::Failure),
            "partial" => Some(Self::Partial),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_edge(confidence: f32) -> Relationship {
        let now = now_epoch();
        Relationship {
            id: "test-edge".to_string(),
            kind: RelationshipKind::Solved,
            confidence,
            provisional: true,
            context: None,
            created_at: now,
            last_confirmed: now,
            access_count: 0,
            success_count: 0,
            failure_count: 0,
        }
    }

    #[test]
    fn test_reinforce_increases_confidence() {
        let mut edge = test_edge(0.3);
        edge.reinforce();
        assert!((edge.confidence - 0.5).abs() < f32::EPSILON);
        assert_eq!(edge.success_count, 1);
        assert!(edge.provisional); // Still provisional after 1 success
    }

    #[test]
    fn test_reinforce_confirms_after_two_successes() {
        let mut edge = test_edge(0.3);
        edge.reinforce();
        edge.reinforce();
        assert!(!edge.provisional);
        assert_eq!(edge.success_count, 2);
    }

    #[test]
    fn test_reinforce_caps_at_one() {
        let mut edge = test_edge(0.95);
        edge.reinforce();
        assert!((edge.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_weaken_decreases_confidence() {
        let mut edge = test_edge(0.5);
        edge.weaken();
        assert!((edge.confidence - 0.35).abs() < f32::EPSILON);
        assert_eq!(edge.failure_count, 1);
    }

    #[test]
    fn test_weaken_floors_at_zero() {
        let mut edge = test_edge(0.1);
        edge.weaken();
        assert!((edge.confidence - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_is_dead_below_threshold() {
        let edge = test_edge(0.03);
        assert!(edge.is_dead(0.05));
    }

    #[test]
    fn test_is_dead_above_threshold() {
        let edge = test_edge(0.1);
        assert!(!edge.is_dead(0.05));
    }

    #[test]
    fn test_entity_type_roundtrip() {
        for t in [
            EntityType::Agent,
            EntityType::Tool,
            EntityType::Problem,
            EntityType::Solution,
            EntityType::Concept,
            EntityType::Person,
            EntityType::Project,
            EntityType::Chunk,
            EntityType::StructFunction,
            EntityType::StructClass,
            EntityType::StructMethod,
            EntityType::StructModule,
            EntityType::StructImport,
        ] {
            assert_eq!(EntityType::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn test_relationship_kind_roundtrip() {
        for k in [
            RelationshipKind::Used,
            RelationshipKind::Solved,
            RelationshipKind::FailedFor,
            RelationshipKind::Knows,
            RelationshipKind::Prefers,
            RelationshipKind::DependsOn,
            RelationshipKind::RelatedTo,
            RelationshipKind::DerivedFrom,
            RelationshipKind::Encountered,
            RelationshipKind::SolvedBy,
            RelationshipKind::Calls,
            RelationshipKind::ImportedBy,
            RelationshipKind::Inherits,
            RelationshipKind::Tests,
            RelationshipKind::Contains,
        ] {
            assert_eq!(RelationshipKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn test_decay_reduces_confidence() {
        let mut edge = test_edge(0.8);
        edge.decay(30.0, 30.0); // 30 days at 30-day half-life = 50%
        assert!((edge.confidence - 0.4).abs() < 0.01);
    }

    #[test]
    fn test_decay_zero_days_no_change() {
        let mut edge = test_edge(0.8);
        edge.decay(0.0, 30.0);
        assert!((edge.confidence - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn test_entity_new_normalizes_label() {
        let entity = Entity::new(
            "n1".to_string(),
            EntityType::Concept,
            "  OAuth  ".to_string(),
        );
        assert_eq!(entity.label, "  OAuth  ");
        assert_eq!(entity.label_normalized, "oauth");
    }

    #[test]
    fn test_outcome_parse() {
        assert_eq!(Outcome::parse("success"), Some(Outcome::Success));
        assert_eq!(Outcome::parse("failure"), Some(Outcome::Failure));
        assert_eq!(Outcome::parse("partial"), Some(Outcome::Partial));
        assert_eq!(Outcome::parse("unknown"), None);
    }
}
