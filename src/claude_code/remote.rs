//! Remote Nellie client for cross-machine Deep Hooks.
//!
//! When Claude Code runs on one machine and Nellie's DB lives on another,
//! the `--server` flag enables sync and ingest to work over HTTP instead
//! of a local SQLite database.
//!
//! - `nellie sync --server http://minidev:8765` queries lessons/checkpoints
//!   via the REST API and writes memory files locally.
//! - `nellie ingest --server http://minidev:8765` parses transcripts locally
//!   and POSTs extracted lessons to the remote server.

use crate::storage::{CheckpointRecord, LessonRecord};

/// Response types matching the server's JSON format.
/// These mirror `src/server/api.rs` structs but are defined here to avoid
/// coupling the CLI to the server module.
#[derive(Debug, serde::Deserialize)]
struct LessonListResponse {
    lessons: Vec<LessonEntry>,
    #[allow(dead_code)]
    total: usize,
}

#[derive(Debug, serde::Deserialize)]
struct LessonEntry {
    id: String,
    title: String,
    content: String,
    severity: String,
    tags: Vec<String>,
    created_at: i64,
}

#[derive(Debug, serde::Deserialize)]
struct CheckpointListResponse {
    checkpoints: Vec<CheckpointEntry>,
    #[allow(dead_code)]
    total: usize,
}

#[derive(Debug, serde::Deserialize)]
struct CheckpointEntry {
    id: String,
    agent: String,
    working_on: String,
    state: serde_json::Value,
    created_at: i64,
}

#[derive(Debug, serde::Deserialize)]
struct AgentListResponse {
    agents: Vec<AgentEntryResponse>,
}

#[derive(Debug, serde::Deserialize)]
struct AgentEntryResponse {
    name: String,
    #[allow(dead_code)]
    checkpoint_count: i64,
    #[allow(dead_code)]
    last_active: i64,
}

#[derive(Debug, serde::Serialize)]
struct CreateLessonRequest {
    title: String,
    content: String,
    severity: String,
    tags: Vec<String>,
}

/// A remote Nellie server client.
pub struct RemoteClient {
    base_url: String,
    client: reqwest::Client,
}

impl RemoteClient {
    /// Create a new remote client pointing at the given Nellie server URL.
    pub fn new(server_url: &str) -> Self {
        let base_url = server_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { base_url, client }
    }

    /// Health check — returns true if the server is reachable.
    pub async fn is_healthy(&self) -> bool {
        self.client
            .get(format!("{}/health", self.base_url))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    /// Fetch lessons from the remote server, sorted by severity priority.
    ///
    /// Queries critical, then warning, then info — matching the local
    /// `query_lessons_by_priority` behavior.
    pub async fn fetch_lessons(&self, max: usize) -> crate::Result<Vec<LessonRecord>> {
        let mut all = Vec::new();

        for severity in &["critical", "warning", "info"] {
            if all.len() >= max {
                break;
            }
            let remaining = max - all.len();
            let url = format!(
                "{}/api/v1/lessons?severity={}&limit={}&offset=0",
                self.base_url, severity, remaining
            );
            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(|e| crate::Error::internal(format!("remote fetch lessons: {e}")))?;

            if !resp.status().is_success() {
                tracing::warn!(
                    status = %resp.status(),
                    severity,
                    "remote server returned non-success for lessons"
                );
                continue;
            }

            let body: LessonListResponse = resp
                .json()
                .await
                .map_err(|e| crate::Error::internal(format!("parse lessons response: {e}")))?;

            all.extend(body.lessons.into_iter().map(|l| LessonRecord {
                id: l.id,
                title: l.title,
                content: l.content,
                tags: l.tags,
                severity: l.severity,
                agent: None,
                repo: None,
                embedding: None,
                created_at: l.created_at,
                updated_at: l.created_at,
            }));
        }

        all.truncate(max);
        Ok(all)
    }

    /// Fetch the latest checkpoint per agent from the remote server.
    pub async fn fetch_latest_checkpoints(
        &self,
        max_agents: usize,
    ) -> crate::Result<Vec<CheckpointRecord>> {
        // First get the agent list
        let agents_url = format!("{}/api/v1/agents", self.base_url);
        let resp = self
            .client
            .get(&agents_url)
            .send()
            .await
            .map_err(|e| crate::Error::internal(format!("remote fetch agents: {e}")))?;

        if !resp.status().is_success() {
            return Ok(Vec::new());
        }

        let agents: AgentListResponse = resp
            .json()
            .await
            .map_err(|e| crate::Error::internal(format!("parse agents response: {e}")))?;

        let mut checkpoints = Vec::new();

        for agent in agents.agents.into_iter().take(max_agents) {
            let cp_url = format!(
                "{}/api/v1/checkpoints?agent={}&limit=1",
                self.base_url,
                urlencoding::encode(&agent.name)
            );
            let resp = self.client.get(&cp_url).send().await;
            let Ok(resp) = resp else { continue };
            if !resp.status().is_success() {
                continue;
            }
            let body: CheckpointListResponse = match resp.json().await {
                Ok(b) => b,
                Err(_) => continue,
            };

            if let Some(cp) = body.checkpoints.into_iter().next() {
                checkpoints.push(CheckpointRecord {
                    id: cp.id,
                    agent: cp.agent,
                    working_on: cp.working_on,
                    state: cp.state,
                    repo: None,
                    session_id: None,
                    created_at: cp.created_at,
                });
            }
        }

        checkpoints.sort_by(|a, b| a.agent.cmp(&b.agent));
        Ok(checkpoints)
    }

    /// Post a new lesson to the remote server.
    pub async fn post_lesson(&self, lesson: &LessonRecord) -> crate::Result<String> {
        let url = format!("{}/api/v1/lessons", self.base_url);
        let body = CreateLessonRequest {
            title: lesson.title.clone(),
            content: lesson.content.clone(),
            severity: lesson.severity.clone(),
            tags: lesson.tags.clone(),
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::Error::internal(format!("remote post lesson: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(crate::Error::internal(format!(
                "remote server returned {status} when creating lesson: {body_text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| crate::Error::internal(format!("parse create lesson response: {e}")))?;

        Ok(result
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_client_url_normalization() {
        let client = RemoteClient::new("http://localhost:8765/");
        assert_eq!(client.base_url, "http://localhost:8765");

        let client = RemoteClient::new("http://localhost:8765");
        assert_eq!(client.base_url, "http://localhost:8765");
    }
}
