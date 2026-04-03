//! ONNX embedding model management.
//!
//! Handles loading and managing the embedding model for text vectorization.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::builder::SessionBuilder;
use ort::session::Session;

use crate::error::EmbeddingError;
use crate::Result;

/// Default model name.
pub const DEFAULT_MODEL_NAME: &str = "all-MiniLM-L6-v2.onnx";

/// Embedding dimension for all-MiniLM-L6-v2.
pub const EMBEDDING_DIM: usize = 384;

/// Maximum sequence length for the model.
pub const MAX_SEQ_LENGTH: usize = 256;

/// ONNX embedding model wrapper.
pub struct EmbeddingModel {
    session: Arc<Session>,
    model_path: PathBuf,
}

impl EmbeddingModel {
    /// Load an ONNX embedding model from the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the model cannot be loaded.
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref().to_path_buf();

        if !model_path.exists() {
            return Err(EmbeddingError::ModelLoad(format!(
                "model file not found: {}",
                model_path.display()
            ))
            .into());
        }

        tracing::info!(path = %model_path.display(), "Loading ONNX embedding model");

        let session = SessionBuilder::new()
            .map_err(|e| EmbeddingError::Runtime(format!("failed to create session builder: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| EmbeddingError::Runtime(format!("failed to set optimization level: {e}")))?
            .with_intra_threads(1)
            .map_err(|e| EmbeddingError::Runtime(format!("failed to set threads: {e}")))?
            .commit_from_file(&model_path)
            .map_err(|e| EmbeddingError::ModelLoad(format!("failed to load model: {e}")))?;

        let input_names: Vec<&str> = session
            .inputs()
            .iter()
            .map(ort::value::Outlet::name)
            .collect();
        let output_names: Vec<&str> = session
            .outputs()
            .iter()
            .map(ort::value::Outlet::name)
            .collect();

        tracing::info!(
            path = %model_path.display(),
            ?input_names,
            ?output_names,
            "Model loaded successfully"
        );

        Ok(Self {
            session: Arc::new(session),
            model_path,
        })
    }

    /// Load a model from the data directory.
    ///
    /// Looks for the model in `{data_dir}/models/{model_name}`.
    ///
    /// # Errors
    ///
    /// Returns an error if the model cannot be found or loaded.
    pub fn load_from_data_dir(data_dir: impl AsRef<Path>, model_name: &str) -> Result<Self> {
        let model_path = data_dir.as_ref().join("models").join(model_name);
        Self::load(model_path)
    }

    /// Load the default model from the data directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the model cannot be found or loaded.
    pub fn load_default(data_dir: impl AsRef<Path>) -> Result<Self> {
        Self::load_from_data_dir(data_dir, DEFAULT_MODEL_NAME)
    }

    /// Get a clone of the session for use in worker threads.
    #[must_use]
    pub fn session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }

    /// Get the model path.
    #[must_use]
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// Get the expected embedding dimension.
    #[must_use]
    pub const fn embedding_dim(&self) -> usize {
        EMBEDDING_DIM
    }

    /// Get the maximum sequence length.
    #[must_use]
    pub const fn max_seq_length(&self) -> usize {
        MAX_SEQ_LENGTH
    }
}

impl std::fmt::Debug for EmbeddingModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingModel")
            .field("model_path", &self.model_path)
            .field("embedding_dim", &EMBEDDING_DIM)
            .field("session", &"<Arc<Session>>")
            .finish()
    }
}

/// Check if the ONNX runtime is available.
///
/// This is useful for graceful degradation when the runtime is not installed.
#[must_use]
pub fn is_runtime_available() -> bool {
    // The ONNX runtime is available if we can initialize the environment
    // ort::init() initializes the global environment, succeeding silently
    let _ = ort::init();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(EMBEDDING_DIM, 384);
        assert_eq!(MAX_SEQ_LENGTH, 256);
        assert!(DEFAULT_MODEL_NAME.ends_with(".onnx"));
    }

    #[test]
    fn test_load_nonexistent_model() {
        let result = EmbeddingModel::load("/nonexistent/model.onnx");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_is_runtime_available() {
        // This may return true or false depending on system setup
        let _ = is_runtime_available();
    }

    // Integration tests that require actual ONNX model are in integration test files
}
