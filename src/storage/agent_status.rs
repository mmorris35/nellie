//! Agent status tracking and management.
//!
//! This module provides functions to track and query agent status,
//! including whether an agent is actively working and what tasks are in progress.

use rusqlite::{params, Connection};

use crate::error::StorageError;
use crate::Result;

/// Get current Unix timestamp as i64.
#[inline]
#[allow(clippy::cast_possible_wrap)]
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Agent status types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is idle, not working on anything.
    Idle,
    /// Agent has work in progress.
    InProgress,
}

impl AgentStatus {
    /// Convert status to string representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::InProgress => "in_progress",
        }
    }

    /// Parse status from string representation.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(Self::Idle),
            "in_progress" => Some(Self::InProgress),
            _ => None,
        }
    }
}

/// Information about an agent's current status.
#[derive(Debug, Clone)]
pub struct AgentStatusInfo {
    /// Name/identifier of the agent.
    pub agent: String,
    /// Current status.
    pub status: AgentStatus,
    /// Current task description if in progress.
    pub current_task: Option<String>,
    /// Unix timestamp of the last status update.
    pub last_updated: i64,
    /// Number of checkpoints for this agent.
    pub checkpoint_count: i64,
}

/// Get the current status of an agent.
///
/// Returns the agent's status including whether they have work in progress.
/// If the agent has never been tracked, returns `Idle` status.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn get_agent_status(conn: &Connection, agent: &str) -> Result<AgentStatusInfo> {
    // Try to get existing status
    let result = conn.query_row(
        "SELECT agent, status, current_task, last_updated FROM agent_status WHERE agent = ?",
        [agent],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    );

    let (agent_name, status_str, current_task, last_updated) = match result {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // Agent not found, create idle entry
            let now = now_unix();

            conn.execute(
                "INSERT OR IGNORE INTO agent_status (agent, status, current_task, last_updated)
                 VALUES (?, ?, ?, ?)",
                params![agent, "idle", None::<String>, now],
            )
            .map_err(|e| StorageError::Database(format!("failed to create agent status: {e}")))?;

            (agent.to_string(), "idle".to_string(), None, now)
        }
        Err(e) => {
            return Err(StorageError::Database(format!("failed to get agent status: {e}")).into());
        }
    };

    let status = AgentStatus::parse(&status_str)
        .ok_or_else(|| StorageError::Database(format!("invalid status: {status_str}")))?;

    // Get checkpoint count for this agent
    let checkpoint_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE agent = ?",
            [agent],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok(AgentStatusInfo {
        agent: agent_name,
        status,
        current_task,
        last_updated,
        checkpoint_count,
    })
}

/// Check if an agent currently has work in progress.
///
/// Returns `true` if the agent is marked as having in-progress work.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn has_in_progress_work(conn: &Connection, agent: &str) -> Result<bool> {
    let status = get_agent_status(conn, agent)?;
    Ok(status.status == AgentStatus::InProgress)
}

/// Mark an agent as having work in progress.
///
/// Updates the agent's status to `InProgress` with an optional task description.
/// If the agent doesn't exist yet, creates a new entry.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn mark_in_progress(conn: &Connection, agent: &str, task: Option<&str>) -> Result<()> {
    let now = now_unix();

    conn.execute(
        "INSERT INTO agent_status (agent, status, current_task, last_updated)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(agent) DO UPDATE SET
            status = excluded.status,
            current_task = excluded.current_task,
            last_updated = excluded.last_updated",
        params![agent, "in_progress", task, now],
    )
    .map_err(|e| StorageError::Database(format!("failed to update agent status: {e}")))?;

    tracing::debug!(agent, task, "Agent marked as in progress");
    Ok(())
}

/// Mark an agent as idle (no work in progress).
///
/// Updates the agent's status to `Idle` and clears the current task.
/// If the agent doesn't exist yet, creates a new entry.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn mark_idle(conn: &Connection, agent: &str) -> Result<()> {
    let now = now_unix();

    conn.execute(
        "INSERT INTO agent_status (agent, status, current_task, last_updated)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(agent) DO UPDATE SET
            status = excluded.status,
            current_task = excluded.current_task,
            last_updated = excluded.last_updated",
        params![agent, "idle", None::<String>, now],
    )
    .map_err(|e| StorageError::Database(format!("failed to update agent status: {e}")))?;

    tracing::debug!(agent, "Agent marked as idle");
    Ok(())
}

/// Get all agents and their current status.
///
/// Returns status information for all tracked agents.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn get_all_agent_statuses(conn: &Connection) -> Result<Vec<AgentStatusInfo>> {
    let mut stmt = conn
        .prepare(
            "SELECT agent, status, current_task, last_updated FROM agent_status ORDER BY last_updated DESC",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let statuses = stmt
        .query_map([], |row| {
            let agent: String = row.get(0)?;
            let status_str: String = row.get(1)?;
            let status = AgentStatus::parse(&status_str).unwrap_or(AgentStatus::Idle);

            Ok((agent, status, row.get::<_, Option<String>>(2)?, row.get(3)?))
        })
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for status_row in statuses {
        let (agent, status, current_task, last_updated) =
            status_row.map_err(|e| StorageError::Database(e.to_string()))?;

        // Get checkpoint count for this agent
        let checkpoint_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM checkpoints WHERE agent = ?",
                [&agent],
                |row| row.get(0),
            )
            .unwrap_or(0);

        result.push(AgentStatusInfo {
            agent,
            status,
            current_task,
            last_updated,
            checkpoint_count,
        });
    }

    Ok(result)
}

/// Get count of agents currently in progress.
///
/// Returns the number of agents that have work in progress.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn count_agents_in_progress(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM agent_status WHERE status = ?",
        ["in_progress"],
        |row| row.get(0),
    )
    .map_err(|e| StorageError::Database(e.to_string()).into())
}

/// Get all agents that are currently in progress.
///
/// Returns status information for agents with in-progress work.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn get_agents_in_progress(conn: &Connection) -> Result<Vec<AgentStatusInfo>> {
    let mut stmt = conn
        .prepare(
            "SELECT agent, status, current_task, last_updated FROM agent_status WHERE status = ? ORDER BY last_updated DESC",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let statuses = stmt
        .query_map(["in_progress"], |row| {
            let agent: String = row.get(0)?;
            let status_str: String = row.get(1)?;
            let status = AgentStatus::parse(&status_str).unwrap_or(AgentStatus::Idle);

            Ok((agent, status, row.get::<_, Option<String>>(2)?, row.get(3)?))
        })
        .map_err(|e| StorageError::Database(e.to_string()))?;

    let mut result = Vec::new();
    for status_row in statuses {
        let (agent, status, current_task, last_updated) =
            status_row.map_err(|e| StorageError::Database(e.to_string()))?;

        // Get checkpoint count for this agent
        let checkpoint_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM checkpoints WHERE agent = ?",
                [&agent],
                |row| row.get(0),
            )
            .unwrap_or(0);

        result.push(AgentStatusInfo {
            agent,
            status,
            current_task,
            last_updated,
            checkpoint_count,
        });
    }

    Ok(result)
}

/// Clear stale agent statuses (agents with last update older than specified duration).
///
/// Removes agents that haven't been updated in more than `max_age_seconds`.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub fn cleanup_stale_statuses(conn: &Connection, max_age_seconds: i64) -> Result<usize> {
    let now = now_unix();
    let cutoff = now - max_age_seconds;

    let deleted = conn
        .execute("DELETE FROM agent_status WHERE last_updated < ?", [cutoff])
        .map_err(|e| StorageError::Database(e.to_string()))?;

    if deleted > 0 {
        tracing::debug!(deleted, "Cleaned up stale agent statuses");
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{migrate, Database};

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            Ok(())
        })
        .unwrap();
        db
    }

    #[test]
    fn test_get_agent_status_new() {
        let db = setup_db();

        db.with_conn(|conn| {
            let status = get_agent_status(conn, "new-agent")?;
            assert_eq!(status.agent, "new-agent");
            assert_eq!(status.status, AgentStatus::Idle);
            assert!(status.current_task.is_none());
            assert_eq!(status.checkpoint_count, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_mark_in_progress() {
        let db = setup_db();

        db.with_conn(|conn| {
            mark_in_progress(conn, "agent1", Some("Working on task X"))?;
            let status = get_agent_status(conn, "agent1")?;
            assert_eq!(status.status, AgentStatus::InProgress);
            assert_eq!(status.current_task, Some("Working on task X".to_string()));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_has_in_progress_work() {
        let db = setup_db();

        db.with_conn(|conn| {
            assert!(!has_in_progress_work(conn, "agent1")?);

            mark_in_progress(conn, "agent1", Some("Task"))?;
            assert!(has_in_progress_work(conn, "agent1")?);

            mark_idle(conn, "agent1")?;
            assert!(!has_in_progress_work(conn, "agent1")?);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_mark_idle() {
        let db = setup_db();

        db.with_conn(|conn| {
            mark_in_progress(conn, "agent1", Some("Task"))?;
            let status = get_agent_status(conn, "agent1")?;
            assert_eq!(status.status, AgentStatus::InProgress);

            mark_idle(conn, "agent1")?;
            let status = get_agent_status(conn, "agent1")?;
            assert_eq!(status.status, AgentStatus::Idle);
            assert!(status.current_task.is_none());

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_get_all_statuses() {
        let db = setup_db();

        db.with_conn(|conn| {
            mark_in_progress(conn, "agent1", Some("Task 1"))?;
            mark_in_progress(conn, "agent2", Some("Task 2"))?;
            mark_idle(conn, "agent3")?;

            let statuses = get_all_agent_statuses(conn)?;
            assert_eq!(statuses.len(), 3);

            let in_progress = statuses
                .iter()
                .filter(|s| s.status == AgentStatus::InProgress);
            assert_eq!(in_progress.count(), 2);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_count_in_progress() {
        let db = setup_db();

        db.with_conn(|conn| {
            assert_eq!(count_agents_in_progress(conn)?, 0);

            mark_in_progress(conn, "agent1", Some("Task 1"))?;
            assert_eq!(count_agents_in_progress(conn)?, 1);

            mark_in_progress(conn, "agent2", Some("Task 2"))?;
            assert_eq!(count_agents_in_progress(conn)?, 2);

            mark_idle(conn, "agent1")?;
            assert_eq!(count_agents_in_progress(conn)?, 1);

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_get_agents_in_progress() {
        let db = setup_db();

        db.with_conn(|conn| {
            mark_in_progress(conn, "agent1", Some("Task 1"))?;
            mark_in_progress(conn, "agent2", Some("Task 2"))?;
            mark_idle(conn, "agent3")?;

            let in_progress = get_agents_in_progress(conn)?;
            assert_eq!(in_progress.len(), 2);

            let agents: Vec<_> = in_progress.iter().map(|s| s.agent.as_str()).collect();
            assert!(agents.contains(&"agent1"));
            assert!(agents.contains(&"agent2"));
            assert!(!agents.contains(&"agent3"));

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_cleanup_stale_statuses() {
        let db = setup_db();

        db.with_conn(|conn| {
            mark_in_progress(conn, "agent1", Some("Task"))?;
            mark_idle(conn, "agent2")?;

            // Update agent2 to an old timestamp (manually for testing)
            let old_timestamp = 1000000000i64; // Very old timestamp
            conn.execute(
                "UPDATE agent_status SET last_updated = ? WHERE agent = ?",
                params![old_timestamp, "agent2"],
            )
            .map_err(|e| StorageError::Database(e.to_string()))?;

            assert_eq!(get_all_agent_statuses(conn)?.len(), 2);

            // Clean up old statuses (older than 365 days)
            cleanup_stale_statuses(conn, 365 * 86400)?;

            // Only agent1 should remain (more recently updated)
            let remaining = get_all_agent_statuses(conn)?;
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0].agent, "agent1");

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_agent_status_enum() {
        assert_eq!(AgentStatus::Idle.as_str(), "idle");
        assert_eq!(AgentStatus::InProgress.as_str(), "in_progress");

        assert_eq!(AgentStatus::parse("idle"), Some(AgentStatus::Idle));
        assert_eq!(
            AgentStatus::parse("in_progress"),
            Some(AgentStatus::InProgress)
        );
        assert_eq!(AgentStatus::parse("invalid"), None);
    }
}
