//! SQLite persistence for the knowledge graph.
//! Loads graph from disk at boot, saves changes incrementally.

use super::entities::{Entity, EntityType, Relationship, RelationshipKind};
use super::memory::GraphMemory;
use crate::config::GraphConfig;
use crate::error::StorageError;
use crate::Result;
use rusqlite::Connection;

/// Load the entire graph from SQLite into a `GraphMemory` instance.
///
/// # Errors
///
/// Returns an error if database queries fail.
pub fn load_graph(conn: &Connection, config: GraphConfig) -> Result<GraphMemory> {
    let mut graph = GraphMemory::new(config);

    // Load all nodes
    let mut stmt = conn
        .prepare(
            "SELECT id, node_type, label, label_normalized, record_id, metadata,
                created_at, last_accessed, access_count
         FROM graph_nodes",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare node query: {e}")))?;

    let nodes = stmt
        .query_map([], |row| {
            Ok(Entity {
                id: row.get(0)?,
                entity_type: EntityType::parse(&row.get::<_, String>(1)?)
                    .unwrap_or(EntityType::Concept),
                label: row.get(2)?,
                label_normalized: row.get(3)?,
                record_id: row.get(4)?,
                metadata: row
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                created_at: row.get(6)?,
                last_accessed: row.get(7)?,
                access_count: row.get::<_, u32>(8).unwrap_or(0),
            })
        })
        .map_err(|e| StorageError::Database(format!("failed to query nodes: {e}")))?;

    for node in nodes {
        let entity =
            node.map_err(|e| StorageError::Database(format!("failed to read node: {e}")))?;
        graph.add_entity(entity);
    }

    // Load all edges
    let mut stmt = conn
        .prepare(
            "SELECT id, from_node, to_node, relationship, confidence, provisional,
                context, created_at, last_confirmed, access_count,
                success_count, failure_count
         FROM graph_edges",
        )
        .map_err(|e| StorageError::Database(format!("failed to prepare edge query: {e}")))?;

    let edges = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?, // from_node
                row.get::<_, String>(2)?, // to_node
                Relationship {
                    id: row.get(0)?,
                    kind: RelationshipKind::parse(&row.get::<_, String>(3)?)
                        .unwrap_or(RelationshipKind::RelatedTo),
                    confidence: row.get(4)?,
                    provisional: row.get::<_, i32>(5)? != 0,
                    context: row.get(6)?,
                    created_at: row.get(7)?,
                    last_confirmed: row.get(8)?,
                    access_count: row.get::<_, u32>(9).unwrap_or(0),
                    success_count: row.get::<_, u32>(10).unwrap_or(0),
                    failure_count: row.get::<_, u32>(11).unwrap_or(0),
                },
            ))
        })
        .map_err(|e| StorageError::Database(format!("failed to query edges: {e}")))?;

    for edge in edges {
        let (from_id, to_id, rel) =
            edge.map_err(|e| StorageError::Database(format!("failed to read edge: {e}")))?;
        graph.add_relationship(&from_id, &to_id, rel);
    }

    Ok(graph)
}

/// Save an entity to SQLite (upsert).
///
/// # Errors
///
/// Returns an error if the database insert fails.
pub fn save_entity(conn: &Connection, entity: &Entity) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO graph_nodes
         (id, node_type, label, label_normalized, record_id, metadata,
          created_at, last_accessed, access_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            entity.id,
            entity.entity_type.as_str(),
            entity.label,
            entity.label_normalized,
            entity.record_id,
            entity
                .metadata
                .as_ref()
                .and_then(|m| serde_json::to_string(m).ok()),
            entity.created_at,
            entity.last_accessed,
            entity.access_count,
        ],
    )
    .map_err(|e| StorageError::Database(format!("failed to save entity: {e}")))?;
    Ok(())
}

/// Save a relationship to SQLite (upsert).
///
/// # Errors
///
/// Returns an error if the database insert fails.
pub fn save_relationship(
    conn: &Connection,
    from_id: &str,
    to_id: &str,
    rel: &Relationship,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO graph_edges
         (id, from_node, to_node, relationship, confidence, provisional,
          context, created_at, last_confirmed, access_count,
          success_count, failure_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            rel.id,
            from_id,
            to_id,
            rel.kind.as_str(),
            rel.confidence,
            i32::from(rel.provisional),
            rel.context,
            rel.created_at,
            rel.last_confirmed,
            rel.access_count,
            rel.success_count,
            rel.failure_count,
        ],
    )
    .map_err(|e| StorageError::Database(format!("failed to save relationship: {e}")))?;
    Ok(())
}

/// Delete an entity and all its edges from SQLite.
///
/// # Errors
///
/// Returns an error if the database delete fails.
pub fn delete_entity(conn: &Connection, entity_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM graph_edges WHERE from_node = ?1 OR to_node = ?1",
        [entity_id],
    )
    .map_err(|e| StorageError::Database(format!("failed to delete entity edges: {e}")))?;
    conn.execute("DELETE FROM graph_nodes WHERE id = ?1", [entity_id])
        .map_err(|e| StorageError::Database(format!("failed to delete entity: {e}")))?;
    Ok(())
}

/// Delete a relationship from SQLite.
///
/// # Errors
///
/// Returns an error if the database delete fails.
pub fn delete_relationship(conn: &Connection, edge_id: &str) -> Result<()> {
    conn.execute("DELETE FROM graph_edges WHERE id = ?1", [edge_id])
        .map_err(|e| StorageError::Database(format!("failed to delete relationship: {e}")))?;
    Ok(())
}

/// Save the entire graph to SQLite (full sync in a transaction).
///
/// # Errors
///
/// Returns an error if any database operation fails. Transaction is rolled back on error.
pub fn save_graph(conn: &Connection, graph: &GraphMemory) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| StorageError::Database(format!("failed to begin transaction: {e}")))?;

    let result = (|| -> Result<()> {
        for entity in graph.all_entities() {
            save_entity(conn, entity)?;
        }
        for (from_id, to_id, rel) in graph.all_relationships() {
            save_relationship(conn, from_id, to_id, rel)?;
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .map_err(|e| StorageError::Database(format!("failed to commit: {e}")))?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::entities::*;
    use crate::storage::migrate;
    use crate::storage::Database;

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| migrate(conn)).unwrap();
        db
    }

    #[test]
    fn test_save_and_load_entity() {
        let db = setup_db();
        let entity = Entity::new("n1".to_string(), EntityType::Tool, "reqwest".to_string());

        db.with_conn(|conn| save_entity(conn, &entity)).unwrap();

        let config = GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        };
        let graph = db.with_conn(|conn| load_graph(conn, config)).unwrap();
        assert_eq!(graph.node_count(), 1);
        let loaded = graph.get_entity("n1").unwrap();
        assert_eq!(loaded.label, "reqwest");
        assert_eq!(loaded.entity_type, EntityType::Tool);
        assert_eq!(loaded.label_normalized, "reqwest");
    }

    #[test]
    fn test_save_and_load_relationship() {
        let db = setup_db();
        let e1 = Entity::new("n1".to_string(), EntityType::Tool, "reqwest".to_string());
        let e2 = Entity::new("n2".to_string(), EntityType::Problem, "timeout".to_string());
        let rel = Relationship::new_provisional(
            "e1".to_string(),
            RelationshipKind::Solved,
            Some("fixed it".to_string()),
        );

        db.with_conn(|conn| {
            save_entity(conn, &e1)?;
            save_entity(conn, &e2)?;
            save_relationship(conn, "n1", "n2", &rel)
        })
        .unwrap();

        let config = GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        };
        let graph = db.with_conn(|conn| load_graph(conn, config)).unwrap();
        assert_eq!(graph.edge_count(), 1);
        let loaded_rel = graph.get_relationship("e1").unwrap();
        assert_eq!(loaded_rel.kind, RelationshipKind::Solved);
        assert!(loaded_rel.provisional);
        assert!((loaded_rel.confidence - 0.3).abs() < f32::EPSILON);
        assert_eq!(loaded_rel.context.as_deref(), Some("fixed it"));
    }

    #[test]
    fn test_full_graph_round_trip() {
        let db = setup_db();

        // Build a graph in memory
        let config = GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        };
        let mut graph = GraphMemory::new(config.clone());
        graph.add_entity(Entity::new(
            "n1".to_string(),
            EntityType::Agent,
            "claude".to_string(),
        ));
        graph.add_entity(Entity::new(
            "n2".to_string(),
            EntityType::Tool,
            "cargo".to_string(),
        ));
        graph.add_entity(Entity::new(
            "n3".to_string(),
            EntityType::Problem,
            "slow build".to_string(),
        ));
        graph.add_relationship(
            "n1",
            "n2",
            Relationship::new_provisional("e1".to_string(), RelationshipKind::Used, None),
        );
        graph.add_relationship(
            "n2",
            "n3",
            Relationship::new_provisional("e2".to_string(), RelationshipKind::Solved, None),
        );

        // Save to SQLite
        db.with_conn(|conn| save_graph(conn, &graph)).unwrap();

        // Load into new graph
        let loaded = db.with_conn(|conn| load_graph(conn, config)).unwrap();
        assert_eq!(loaded.node_count(), 3);
        assert_eq!(loaded.edge_count(), 2);
        assert!(loaded.get_entity("n1").is_some());
        assert!(loaded.get_entity("n2").is_some());
        assert!(loaded.get_entity("n3").is_some());
        assert!(loaded.get_relationship("e1").is_some());
        assert!(loaded.get_relationship("e2").is_some());
    }

    #[test]
    fn test_delete_entity_cascades_edges() {
        let db = setup_db();
        let e1 = Entity::new("n1".to_string(), EntityType::Agent, "claude".to_string());
        let e2 = Entity::new("n2".to_string(), EntityType::Tool, "cargo".to_string());
        let rel = Relationship::new_provisional("e1".to_string(), RelationshipKind::Used, None);

        db.with_conn(|conn| {
            save_entity(conn, &e1)?;
            save_entity(conn, &e2)?;
            save_relationship(conn, "n1", "n2", &rel)?;
            delete_entity(conn, "n1")
        })
        .unwrap();

        let config = GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        };
        let graph = db.with_conn(|conn| load_graph(conn, config)).unwrap();
        assert_eq!(graph.node_count(), 1); // Only n2 remains
        assert_eq!(graph.edge_count(), 0); // Edge was cascaded
    }

    #[test]
    fn test_load_empty_database() {
        let db = setup_db();
        let config = GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        };
        let graph = db.with_conn(|conn| load_graph(conn, config)).unwrap();
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_save_entity_upsert() {
        let db = setup_db();
        let mut entity = Entity::new("n1".to_string(), EntityType::Tool, "reqwest".to_string());
        db.with_conn(|conn| save_entity(conn, &entity)).unwrap();

        // Update and re-save
        entity.access_count = 5;
        db.with_conn(|conn| save_entity(conn, &entity)).unwrap();

        let config = GraphConfig {
            enabled: true,
            ..GraphConfig::default()
        };
        let graph = db.with_conn(|conn| load_graph(conn, config)).unwrap();
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.get_entity("n1").unwrap().access_count, 5);
    }
}
