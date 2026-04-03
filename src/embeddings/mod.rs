//! ONNX-based embedding generation.
//!
//! This module provides:
//! - ONNX Runtime integration via the `ort` crate
//! - Dedicated thread pool for embedding generation
//! - Async API using channels for non-blocking operation

mod model;
mod service;
mod worker;

pub use model::{
    is_runtime_available, EmbeddingModel, DEFAULT_MODEL_NAME, EMBEDDING_DIM, MAX_SEQ_LENGTH,
};
pub use service::{placeholder_embedding, EmbeddingConfig, EmbeddingService};
pub use worker::{load_tokenizer, EmbeddingWorker};

/// Initialize embeddings module.
pub fn init() {
    if is_runtime_available() {
        tracing::info!("ONNX runtime available");
    } else {
        tracing::warn!("ONNX runtime not available - embeddings will be disabled");
    }
}
