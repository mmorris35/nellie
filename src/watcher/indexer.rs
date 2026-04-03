//! Incremental indexing service.

use std::path::Path;
use std::sync::Arc;

use blake3::Hasher;
use tokio::sync::mpsc;

use super::chunker::Chunker;
use super::handler::IndexRequest;
use crate::embeddings::EmbeddingService;
use crate::storage::{delete_chunks_by_file, insert_chunk, ChunkRecord, Database};
use crate::Result;

/// Indexer service that processes files and stores chunks.
pub struct Indexer {
    db: Database,
    embeddings: Option<EmbeddingService>,
    chunker: Chunker,
    enable_structural: bool,
}

impl Indexer {
    /// Create a new indexer.
    #[must_use]
    pub fn new(
        db: Database,
        embeddings: Option<EmbeddingService>,
        enable_structural: bool,
    ) -> Self {
        Self {
            db,
            embeddings,
            chunker: Chunker::default_chunker(),
            enable_structural,
        }
    }

    /// Index a single file.
    ///
    /// # Errors
    ///
    /// Returns an error if indexing fails.
    pub async fn index_file(&self, request: &IndexRequest) -> Result<usize> {
        let path = &request.path;

        if !path.exists() {
            tracing::warn!(path = %path.display(), "File no longer exists");
            return Ok(0);
        }

        // Read file content
        let content = tokio::fs::read_to_string(path).await?;
        let file_hash = compute_hash(&content);

        // Check if already indexed with same hash
        if self.is_already_indexed(path, &file_hash)? {
            tracing::debug!(path = %path.display(), "File unchanged, skipping");
            return Ok(0);
        }

        // Remove old chunks
        self.db.with_conn(|conn| {
            delete_chunks_by_file(conn, &path.to_string_lossy())?;
            Ok(())
        })?;

        // Chunk the file
        let chunks = self
            .chunker
            .chunk_content(&content, request.language.as_deref());

        if chunks.is_empty() {
            return Ok(0);
        }

        // Generate embeddings
        let embeddings = self.generate_embeddings(&chunks).await?;

        // Store chunks
        let path_str = path.to_string_lossy().to_string();
        let mut count = 0;

        for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
            #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
            let mut record = ChunkRecord::new(
                &path_str,
                chunk.index as i32,
                chunk.start_line as i32,
                chunk.end_line as i32,
                &chunk.content,
                &file_hash,
            )
            .with_language(request.language.clone().unwrap_or_default());

            // Only add embedding if we have a real embedding service (not placeholder)
            if self.embeddings.is_some() {
                record = record.with_embedding(embedding.clone());
            }

            self.db.with_conn(|conn| {
                insert_chunk(conn, &record)?;
                Ok(())
            })?;

            count += 1;
        }

        // Structural parsing
        if self.enable_structural {
            if let Some(lang) = crate::structural::SupportedLanguage::from_path(path) {
                // Check if symbols already exist for this file+hash
                let already_indexed = self
                    .db
                    .with_conn(|conn| {
                        crate::structural::storage::has_symbols_for_hash(
                            conn, &path_str, &file_hash,
                        )
                    })
                    .unwrap_or(false);

                if already_indexed {
                    tracing::debug!(path = %path.display(), "Structural symbols up to date, skipping");
                } else {
                    match crate::structural::extract_symbols(path, content.as_bytes(), lang) {
                        Ok(result) => {
                            self.db.with_conn(|conn| {
                                crate::structural::storage::store_symbols(
                                    conn,
                                    &path_str,
                                    &file_hash,
                                    &result.symbols,
                                )?;

                                // Store call edges
                                for symbol in &result.symbols {
                                    if symbol.kind == crate::structural::SymbolKind::CallSite {
                                        if let Some(scope) = &symbol.scope {
                                            // Find the source symbol ID (the containing function)
                                            let source_symbols =
                                                crate::structural::storage::query_symbols_by_name(
                                                    conn, scope,
                                                )?;
                                            if let Some(source_sym) = source_symbols.first() {
                                                crate::structural::storage::store_structural_edges(
                                                    conn,
                                                    source_sym.id,
                                                    &[(
                                                        symbol.name.clone(),
                                                        None,
                                                        "calls".to_string(),
                                                    )],
                                                )?;
                                            }
                                        }
                                    }
                                }
                                Ok(())
                            })?;

                            tracing::debug!(
                                path = %path.display(),
                                symbols = result.symbols.len(),
                                "Structural analysis complete"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "Structural analysis failed, continuing"
                            );
                        }
                    }
                }
            }
        }

        // Update file state
        self.update_file_state(path, &file_hash)?;

        tracing::info!(
            path = %path.display(),
            chunks = count,
            "Indexed file"
        );

        Ok(count)
    }

    /// Delete index for a file.
    ///
    /// # Errors
    ///
    /// Returns an error if deletion fails.
    pub fn delete_file(&self, path: &Path) -> Result<usize> {
        let path_str = path.to_string_lossy().to_string();

        let deleted = self.db.with_conn(|conn| {
            let count = delete_chunks_by_file(conn, &path_str)?;
            // Remove file state
            conn.execute("DELETE FROM file_state WHERE path = ?", [&path_str])
                .ok();
            Ok(count)
        })?;

        if deleted > 0 {
            tracing::info!(path = %path.display(), chunks = deleted, "Deleted file from index");
        }

        Ok(deleted)
    }

    /// Check if file is already indexed with same hash.
    fn is_already_indexed(&self, path: &Path, hash: &str) -> Result<bool> {
        let path_str = path.to_string_lossy();

        self.db.with_conn(|conn| {
            let result: std::result::Result<String, rusqlite::Error> = conn.query_row(
                "SELECT hash FROM file_state WHERE path = ?",
                [&*path_str],
                |row| row.get(0),
            );

            Ok(result.is_ok_and(|stored_hash| stored_hash == hash))
        })
    }

    /// Update file state after indexing.
    fn update_file_state(&self, path: &Path, hash: &str) -> Result<()> {
        let metadata = std::fs::metadata(path)?;
        #[allow(clippy::cast_possible_wrap)]
        let mtime = metadata
            .modified()
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            })
            .unwrap_or(0);
        #[allow(clippy::cast_possible_wrap)]
        let size = metadata.len() as i64;
        let path_str = path.to_string_lossy().to_string();
        #[allow(clippy::cast_possible_wrap)]
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO file_state (path, mtime, size, hash, last_indexed) \
                 VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![&path_str, mtime, size, hash, now],
            )
            .map_err(|e| {
                crate::Error::Storage(crate::error::StorageError::Database(e.to_string()))
            })?;
            Ok(())
        })
    }

    /// Generate embeddings for chunks.
    async fn generate_embeddings(
        &self,
        chunks: &[super::chunker::CodeChunk],
    ) -> Result<Vec<Vec<f32>>> {
        if let Some(ref service) = self.embeddings {
            if service.is_initialized() {
                let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
                return service.embed_batch(texts).await;
            }
        }

        // Return empty embeddings when no service available
        // (will not be stored, so no vector table needed)
        Ok(vec![vec![]; chunks.len()])
    }

    /// Run the indexer loop processing requests from a channel.
    pub async fn run(
        self: Arc<Self>,
        mut index_rx: mpsc::Receiver<IndexRequest>,
        mut delete_rx: mpsc::Receiver<std::path::PathBuf>,
    ) {
        tracing::info!("Indexer started");

        loop {
            tokio::select! {
                Some(request) = index_rx.recv() => {
                    if let Err(e) = self.index_file(&request).await {
                        tracing::error!(path = %request.path.display(), error = %e, "Failed to index file");
                    }
                }
                Some(path) = delete_rx.recv() => {
                    if let Err(e) = self.delete_file(&path) {
                        tracing::error!(path = %path.display(), error = %e, "Failed to delete file from index");
                    }
                }
                else => {
                    tracing::info!("Indexer channels closed, shutting down");
                    break;
                }
            }
        }
    }
}

/// Compute blake3 hash of content.
fn compute_hash(content: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{migrate, Database};
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            // Skip vector table creation for unit tests (sqlite-vec not available in memory)
            Ok(())
        })
        .unwrap();
        db
    }

    #[tokio::test]
    async fn test_index_file() {
        let db = setup_test_db();
        let indexer = Indexer::new(db.clone(), None, false);

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "fn main() {\n    println!(\"hello\");\n}").unwrap();

        let request = IndexRequest {
            path: file_path.clone(),
            language: Some("rust".to_string()),
        };

        let count = indexer.index_file(&request).await.unwrap();
        assert!(count > 0);

        // Verify chunks in database
        let chunks = db
            .with_conn(|conn| {
                crate::storage::get_chunks_by_file(conn, &file_path.to_string_lossy())
            })
            .unwrap();

        assert!(!chunks.is_empty());
    }

    #[tokio::test]
    async fn test_reindex_unchanged() {
        let db = setup_test_db();
        let indexer = Indexer::new(db, None, false);

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        let request = IndexRequest {
            path: file_path.clone(),
            language: Some("rust".to_string()),
        };

        // First index
        let count1 = indexer.index_file(&request).await.unwrap();
        assert!(count1 > 0);

        // Second index (unchanged)
        let count2 = indexer.index_file(&request).await.unwrap();
        assert_eq!(count2, 0); // Should skip
    }

    #[tokio::test]
    async fn test_delete_file() {
        let db = setup_test_db();
        let indexer = Indexer::new(db.clone(), None, false);

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        let request = IndexRequest {
            path: file_path.clone(),
            language: Some("rust".to_string()),
        };

        indexer.index_file(&request).await.unwrap();

        let deleted = indexer.delete_file(&file_path).unwrap();
        assert!(deleted > 0);

        // Verify empty
        let chunks = db
            .with_conn(|conn| {
                crate::storage::get_chunks_by_file(conn, &file_path.to_string_lossy())
            })
            .unwrap();

        assert!(chunks.is_empty());
    }

    #[test]
    fn test_compute_hash() {
        let hash1 = compute_hash("hello");
        let hash2 = compute_hash("hello");
        let hash3 = compute_hash("world");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64); // blake3 hex is 64 chars
    }
}
