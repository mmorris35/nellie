//! ONNX-based embedding generation.
//!
//! This module provides:
//! - ONNX Runtime integration via the `ort` crate
//! - Dedicated thread pool for embedding generation
//! - Async API using channels for non-blocking operation

mod model;
mod service;
mod spec;
pub mod version;
mod worker;

pub use model::{
    is_runtime_available, EmbeddingModel, DEFAULT_MODEL_NAME, EMBEDDING_DIM, MAX_SEQ_LENGTH,
};
pub use service::{placeholder_embedding, EmbeddingConfig, EmbeddingService};
pub use spec::{
    checkpoint_embedding_text, chunk_embedding_text, lesson_embedding_text, lesson_section_texts,
    EmbeddingSpec, LESSON_SECTION_OVERLAP, LESSON_SECTION_TOKENS, LESSON_VECTORS_SECTIONS,
    LESSON_VECTORS_WHOLE, MODEL_ID, NORMALISATION_L2, SPECIAL_TOKENS_INTACT,
    SPECIAL_TOKENS_TRUNCATED,
};
pub use worker::{configure_tokenizer, load_tokenizer, EmbeddingWorker};

/// Initialize embeddings module.
pub fn init() {
    if is_runtime_available() {
        tracing::info!("ONNX runtime available");
    } else {
        tracing::warn!("ONNX runtime not available - embeddings will be disabled");
    }
}
