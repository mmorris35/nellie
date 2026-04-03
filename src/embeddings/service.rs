//! High-level embedding service.
//!
//! Provides a convenient async API for generating embeddings.

use std::path::Path;
use std::sync::Arc;

use tokenizers::Tokenizer;
use tokio::sync::RwLock;

use super::model::EmbeddingModel;
use super::worker::EmbeddingWorker;
use crate::error::EmbeddingError;
use crate::Result;

/// Embedding service configuration.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Path to ONNX model file.
    pub model_path: std::path::PathBuf,

    /// Path to tokenizer.json file.
    pub tokenizer_path: std::path::PathBuf,

    /// Number of worker threads.
    pub num_workers: usize,
}

impl EmbeddingConfig {
    /// Create config from data directory.
    ///
    /// Expects model at `{data_dir}/models/all-MiniLM-L6-v2.onnx`
    /// and tokenizer at `{data_dir}/models/tokenizer.json`.
    #[must_use]
    pub fn from_data_dir(data_dir: impl AsRef<Path>, num_workers: usize) -> Self {
        let models_dir = data_dir.as_ref().join("models");
        Self {
            model_path: models_dir.join("all-MiniLM-L6-v2.onnx"),
            tokenizer_path: models_dir.join("tokenizer.json"),
            num_workers,
        }
    }
}

/// High-level embedding service.
///
/// Thread-safe and can be cloned cheaply.
#[derive(Clone)]
pub struct EmbeddingService {
    inner: Arc<EmbeddingServiceInner>,
}

struct EmbeddingServiceInner {
    worker: RwLock<Option<EmbeddingWorker>>,
    config: EmbeddingConfig,
    initialized: std::sync::atomic::AtomicBool,
}

impl EmbeddingService {
    /// Create a new embedding service.
    ///
    /// The service is created but not initialized. Call `init()` to start workers.
    #[must_use]
    pub fn new(config: EmbeddingConfig) -> Self {
        Self {
            inner: Arc::new(EmbeddingServiceInner {
                worker: RwLock::new(None),
                config,
                initialized: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Initialize the embedding service.
    ///
    /// Loads the model and starts worker threads.
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails.
    pub async fn init(&self) -> Result<()> {
        {
            let mut worker_guard = self.inner.worker.write().await;

            if worker_guard.is_some() {
                return Ok(()); // Already initialized
            }

            tracing::info!("Initializing embedding service");

            // Load model and extract session (drop model so Arc refcount = 1
            // for try_unwrap in the worker pool)
            let model = EmbeddingModel::load(&self.inner.config.model_path)?;
            let session = model.session();
            drop(model);

            // Load tokenizer
            let tokenizer =
                Tokenizer::from_file(&self.inner.config.tokenizer_path).map_err(|e| {
                    EmbeddingError::Tokenization(format!("failed to load tokenizer: {e}"))
                })?;

            // Create worker pool
            let worker =
                EmbeddingWorker::new(session, Arc::new(tokenizer), self.inner.config.num_workers)?;

            *worker_guard = Some(worker);
        }
        self.inner
            .initialized
            .store(true, std::sync::atomic::Ordering::Release);

        tracing::info!("Embedding service initialized");
        Ok(())
    }

    /// Check if the service is initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.inner
            .initialized
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Generate embedding for a single text.
    ///
    /// # Errors
    ///
    /// Returns an error if not initialized or embedding fails.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn embed_one(&self, text: impl Into<String>) -> Result<Vec<f32>> {
        {
            let worker_guard = self.inner.worker.read().await;
            let worker = worker_guard
                .as_ref()
                .ok_or_else(|| EmbeddingError::WorkerPool("service not initialized".to_string()))?;
            worker.embed_one(text.into()).await
        }
    }

    /// Generate embeddings for multiple texts.
    ///
    /// # Errors
    ///
    /// Returns an error if not initialized or embedding fails.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        {
            let worker_guard = self.inner.worker.read().await;
            let worker = worker_guard
                .as_ref()
                .ok_or_else(|| EmbeddingError::WorkerPool("service not initialized".to_string()))?;
            worker.embed(texts).await
        }
    }

    /// Generate embeddings for texts, returning results paired with original texts.
    ///
    /// # Errors
    ///
    /// Returns an error if not initialized or embedding fails.
    pub async fn embed_with_texts(&self, texts: Vec<String>) -> Result<Vec<(String, Vec<f32>)>> {
        let embeddings = self.embed_batch(texts.clone()).await?;
        Ok(texts.into_iter().zip(embeddings).collect())
    }
}

impl std::fmt::Debug for EmbeddingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingService")
            .field("initialized", &self.is_initialized())
            .field("config", &self.inner.config)
            .finish()
    }
}

/// Create a placeholder embedding for testing.
///
/// Returns a deterministic embedding based on the text hash.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn placeholder_embedding(text: &str) -> Vec<f32> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();

    // Generate a deterministic 384-dim vector from hash
    let mut embedding = Vec::with_capacity(super::model::EMBEDDING_DIM);
    let mut seed = hash;
    for _ in 0..super::model::EMBEDDING_DIM {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let value = (((seed >> 33) as f32) / (u32::MAX as f32)).mul_add(2.0, -1.0);
        embedding.push(value);
    }

    // L2 normalize
    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut embedding {
            *v /= norm;
        }
    }

    embedding
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_config_from_data_dir() {
        let config = EmbeddingConfig::from_data_dir("/var/lib/nellie", 4);
        assert_eq!(
            config.model_path.to_string_lossy(),
            "/var/lib/nellie/models/all-MiniLM-L6-v2.onnx"
        );
        assert_eq!(
            config.tokenizer_path.to_string_lossy(),
            "/var/lib/nellie/models/tokenizer.json"
        );
        assert_eq!(config.num_workers, 4);
    }

    #[test]
    fn test_service_not_initialized() {
        let config = EmbeddingConfig::from_data_dir("/tmp", 1);
        let service = EmbeddingService::new(config);
        assert!(!service.is_initialized());
    }

    #[test]
    fn test_placeholder_embedding() {
        let emb1 = placeholder_embedding("hello world");
        let emb2 = placeholder_embedding("hello world");
        let emb3 = placeholder_embedding("different text");

        // Same text produces same embedding
        assert_eq!(emb1, emb2);

        // Different text produces different embedding
        assert_ne!(emb1, emb3);

        // Correct dimension
        assert_eq!(emb1.len(), super::super::model::EMBEDDING_DIM);

        // Is normalized (L2 norm ≈ 1)
        let norm: f32 = emb1.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_embed_without_init() {
        let config = EmbeddingConfig::from_data_dir("/tmp", 1);
        let service = EmbeddingService::new(config);

        let result = service.embed_one("test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not initialized"));
    }
}
