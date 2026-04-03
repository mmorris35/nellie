//! Nellie-V: Graph-based knowledge memory layer.
//!
//! Adds relationship-aware memory on top of existing vector search.
//! Feature-gated behind `GraphConfig::enabled`.

pub mod bootstrap;
pub mod enrichment;
pub mod entities;
pub mod integrity;
pub mod matching;
pub mod memory;
pub mod persistence;
pub mod query;

pub use bootstrap::{process_checkpoint, process_lesson, run_bootstrap, BootstrapStats};
pub use enrichment::{ensure_edge, ensure_entity, persist_changes};
pub use entities::{Entity, EntityType, Outcome, Relationship, RelationshipKind};
pub use matching::{find_best_match, normalize_label, MatchResult, MatchType};
pub use memory::GraphMemory;
pub use query::{Direction, EdgeSummary, EntitySummary, GraphQuery, QueryResult};
