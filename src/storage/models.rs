//! Data models for storage operations.
//!
//! This module defines the core data structures used for:
//! - Code chunks with embeddings
//! - Lessons learned
//! - Agent checkpoints
//! - File state tracking
//! - Search results

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Get current Unix timestamp.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
        .unwrap_or(0)
}

/// Generate a unique ID with a given prefix.
fn generate_id(prefix: &str) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    format!("{}_{:x}", prefix, hasher.finish())
}

/// A code chunk with its embedding and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    /// Unique identifier (database primary key).
    pub id: Option<i64>,

    /// Full path to the source file.
    pub file_path: String,

    /// Index of this chunk within the file (0-based).
    pub chunk_index: i32,

    /// Starting line number (1-based).
    pub start_line: i32,

    /// Ending line number (1-based, inclusive).
    pub end_line: i32,

    /// The actual code content.
    pub content: String,

    /// Programming language (e.g., "rust", "python").
    pub language: Option<String>,

    /// Hash of the source file for change detection.
    pub file_hash: String,

    /// Unix timestamp when this chunk was indexed.
    pub indexed_at: i64,

    /// Embedding vector (384 dimensions for all-MiniLM-L6-v2).
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
}

impl ChunkRecord {
    /// Create a new chunk record.
    #[must_use]
    pub fn new(
        file_path: impl Into<String>,
        chunk_index: i32,
        start_line: i32,
        end_line: i32,
        content: impl Into<String>,
        file_hash: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            file_path: file_path.into(),
            chunk_index,
            start_line,
            end_line,
            content: content.into(),
            language: None,
            file_hash: file_hash.into(),
            indexed_at: now_unix(),
            embedding: None,
        }
    }

    /// Set the programming language.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Set the embedding vector.
    #[must_use]
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    /// Get line count for this chunk.
    #[must_use]
    pub const fn line_count(&self) -> i32 {
        self.end_line - self.start_line + 1
    }
}

/// A lesson learned entry with semantic search capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonRecord {
    /// Unique identifier.
    pub id: String,

    /// Brief title.
    pub title: String,

    /// Full content/description.
    pub content: String,

    /// Tags for categorization.
    pub tags: Vec<String>,

    /// Severity level: "critical", "warning", or "info".
    pub severity: String,

    /// Agent that created this lesson (optional).
    pub agent: Option<String>,

    /// Repository this lesson relates to (optional).
    pub repo: Option<String>,

    /// Unix timestamp when created.
    pub created_at: i64,

    /// Unix timestamp when last updated.
    pub updated_at: i64,

    /// Embedding vector for semantic search.
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
}

impl LessonRecord {
    /// Create a new lesson record.
    #[must_use]
    pub fn new(title: impl Into<String>, content: impl Into<String>, tags: Vec<String>) -> Self {
        let now = now_unix();
        Self {
            id: generate_id("lesson"),
            title: title.into(),
            content: content.into(),
            tags,
            severity: "info".to_string(),
            agent: None,
            repo: None,
            created_at: now,
            updated_at: now,
            embedding: None,
        }
    }

    /// Set the severity level.
    #[must_use]
    pub fn with_severity(mut self, severity: impl Into<String>) -> Self {
        self.severity = severity.into();
        self
    }

    /// Set the agent that created this lesson.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Set the repository this lesson relates to.
    #[must_use]
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// Set the embedding vector.
    #[must_use]
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }
}

/// An agent checkpoint for saving/restoring working state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    /// Unique identifier.
    pub id: String,

    /// Agent identifier.
    pub agent: String,

    /// Description of what the agent was working on.
    pub working_on: String,

    /// Agent state as JSON.
    pub state: serde_json::Value,

    /// Repository context (optional).
    pub repo: Option<String>,

    /// Session identifier (optional).
    pub session_id: Option<String>,

    /// Unix timestamp when created.
    pub created_at: i64,
}

impl CheckpointRecord {
    /// Create a new checkpoint record.
    #[must_use]
    pub fn new(
        agent: impl Into<String>,
        working_on: impl Into<String>,
        state: serde_json::Value,
    ) -> Self {
        Self {
            id: generate_id("checkpoint"),
            agent: agent.into(),
            working_on: working_on.into(),
            state,
            repo: None,
            session_id: None,
            created_at: now_unix(),
        }
    }

    /// Set the repository context.
    #[must_use]
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// Set the session identifier.
    #[must_use]
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// File state for incremental indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    /// Full path to the file.
    pub path: String,

    /// Modification timestamp (Unix seconds).
    pub mtime: i64,

    /// File size in bytes.
    pub size: i64,

    /// Hash of file content (SHA256 hex string).
    pub hash: String,

    /// Unix timestamp when last indexed.
    pub last_indexed: i64,
}

impl FileState {
    /// Create a new file state record.
    #[must_use]
    pub fn new(path: impl Into<String>, mtime: i64, size: i64, hash: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            mtime,
            size,
            hash: hash.into(),
            last_indexed: now_unix(),
        }
    }
}

/// Search result with similarity score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult<T> {
    /// The matching record.
    pub record: T,

    /// Raw distance from query embedding (0 = perfect match, 2 = opposite).
    pub distance: f32,

    /// Normalized similarity score (0.0 = opposite, 1.0 = perfect match).
    pub score: f32,
}

impl<T> SearchResult<T> {
    /// Create a new search result.
    ///
    /// Converts distance to score using: `score = 1.0 - (distance / 2.0)`
    /// This maps the distance range [0, 2] to score range [1, 0].
    #[must_use]
    pub fn new(record: T, distance: f32) -> Self {
        let score = (1.0 - (distance / 2.0)).clamp(0.0, 1.0);
        Self {
            record,
            distance,
            score,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_record_new() {
        let chunk = ChunkRecord::new("/test/file.rs", 0, 1, 10, "fn main() {}", "abc123");

        assert!(chunk.id.is_none());
        assert_eq!(chunk.file_path, "/test/file.rs");
        assert_eq!(chunk.chunk_index, 0);
        assert_eq!(chunk.start_line, 1);
        assert_eq!(chunk.end_line, 10);
        assert_eq!(chunk.content, "fn main() {}");
        assert_eq!(chunk.file_hash, "abc123");
        assert!(chunk.indexed_at > 0);
        assert!(chunk.embedding.is_none());
    }

    #[test]
    fn test_chunk_record_builder() {
        let chunk = ChunkRecord::new("/test", 0, 1, 5, "code", "hash")
            .with_language("rust")
            .with_embedding(vec![0.1, 0.2, 0.3]);

        assert_eq!(chunk.language, Some("rust".to_string()));
        assert!(chunk.embedding.is_some());
        assert_eq!(chunk.embedding.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn test_chunk_line_count() {
        let chunk = ChunkRecord::new("/test", 0, 5, 15, "content", "hash");
        assert_eq!(chunk.line_count(), 11); // 15 - 5 + 1
    }

    #[test]
    fn test_lesson_record_new() {
        let lesson = LessonRecord::new(
            "Test Lesson",
            "This is a test",
            vec!["rust".to_string(), "testing".to_string()],
        );

        assert!(lesson.id.starts_with("lesson_"));
        assert_eq!(lesson.title, "Test Lesson");
        assert_eq!(lesson.content, "This is a test");
        assert_eq!(lesson.tags.len(), 2);
        assert_eq!(lesson.severity, "info");
        assert!(lesson.created_at > 0);
    }

    #[test]
    fn test_lesson_record_builder() {
        let lesson = LessonRecord::new("Title", "Content", vec![])
            .with_severity("critical")
            .with_agent("test-agent")
            .with_repo("test-repo");

        assert_eq!(lesson.severity, "critical");
        assert_eq!(lesson.agent, Some("test-agent".to_string()));
        assert_eq!(lesson.repo, Some("test-repo".to_string()));
    }

    #[test]
    fn test_checkpoint_record_new() {
        let state = serde_json::json!({"key": "value"});
        let checkpoint = CheckpointRecord::new("test-agent", "Working on feature X", state.clone());

        assert!(checkpoint.id.starts_with("checkpoint_"));
        assert_eq!(checkpoint.agent, "test-agent");
        assert_eq!(checkpoint.working_on, "Working on feature X");
        assert_eq!(checkpoint.state, state);
    }

    #[test]
    fn test_checkpoint_record_builder() {
        let checkpoint = CheckpointRecord::new("agent", "working", serde_json::json!({}))
            .with_repo("test-repo")
            .with_session("session-123");

        assert_eq!(checkpoint.repo, Some("test-repo".to_string()));
        assert_eq!(checkpoint.session_id, Some("session-123".to_string()));
    }

    #[test]
    fn test_file_state_new() {
        let state = FileState::new("/test/file.rs", 1234567890, 1024, "abc123");

        assert_eq!(state.path, "/test/file.rs");
        assert_eq!(state.mtime, 1234567890);
        assert_eq!(state.size, 1024);
        assert_eq!(state.hash, "abc123");
        assert!(state.last_indexed > 0);
    }

    #[test]
    fn test_search_result_score() {
        let chunk = ChunkRecord::new("/test", 0, 1, 1, "content", "hash");

        // Perfect match (distance = 0)
        let result = SearchResult::new(chunk.clone(), 0.0);
        assert!((result.score - 1.0).abs() < 0.001);

        // Halfway (distance = 1)
        let result = SearchResult::new(chunk.clone(), 1.0);
        assert!((result.score - 0.5).abs() < 0.001);

        // Opposite (distance = 2)
        let result = SearchResult::new(chunk, 2.0);
        assert!((result.score - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_chunk_serialization() {
        let chunk = ChunkRecord::new("/test/file.rs", 0, 1, 10, "fn main() {}", "abc123")
            .with_language("rust");

        let json = serde_json::to_string(&chunk).unwrap();
        let deserialized: ChunkRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(chunk.file_path, deserialized.file_path);
        assert_eq!(chunk.content, deserialized.content);
        assert_eq!(chunk.language, deserialized.language);
        // Embedding is skipped in serialization
        assert!(deserialized.embedding.is_none());
    }

    #[test]
    fn test_unique_ids() {
        let lesson1 = LessonRecord::new("L1", "C1", vec![]);
        let lesson2 = LessonRecord::new("L2", "C2", vec![]);

        assert_ne!(lesson1.id, lesson2.id);

        let cp1 = CheckpointRecord::new("a", "w", serde_json::json!({}));
        let cp2 = CheckpointRecord::new("a", "w", serde_json::json!({}));

        assert_ne!(cp1.id, cp2.id);
    }
}
