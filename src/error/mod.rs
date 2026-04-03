//! Error types and Result aliases for Nellie.
//!
//! This module defines the error hierarchy used throughout the crate.
//! All public functions return `Result<T, Error>` or `Result<T>`.

use thiserror::Error;

/// Result type alias using Nellie's Error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for Nellie operations.
#[derive(Error, Debug)]
pub enum Error {
    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// Database/storage error.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// Embedding generation error.
    #[error("embedding error: {0}")]
    Embedding(#[from] EmbeddingError),

    /// File watching error.
    #[error("watcher error: {0}")]
    Watcher(#[from] WatcherError),

    /// Server/API error.
    #[error("server error: {0}")]
    Server(#[from] ServerError),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic internal error.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Storage-specific errors.
#[derive(Error, Debug)]
pub enum StorageError {
    /// `SQLite` database error.
    #[error("database error: {0}")]
    Database(String),

    /// Record not found.
    #[error("not found: {entity} with id '{id}'")]
    NotFound { entity: &'static str, id: String },

    /// Schema migration error.
    #[error("migration error: {0}")]
    Migration(String),

    /// Vector operation error.
    #[error("vector error: {0}")]
    Vector(String),
}

/// Embedding-specific errors.
#[derive(Error, Debug)]
pub enum EmbeddingError {
    /// ONNX runtime error.
    #[error("ONNX runtime error: {0}")]
    Runtime(String),

    /// Model loading error.
    #[error("failed to load model: {0}")]
    ModelLoad(String),

    /// Tokenization error.
    #[error("tokenization error: {0}")]
    Tokenization(String),

    /// Worker pool error.
    #[error("worker pool error: {0}")]
    WorkerPool(String),
}

/// File watcher errors.
#[derive(Error, Debug)]
pub enum WatcherError {
    /// Failed to watch path.
    #[error("failed to watch path '{path}': {reason}")]
    WatchFailed { path: String, reason: String },

    /// File processing error.
    #[error("failed to process file '{path}': {reason}")]
    ProcessFailed { path: String, reason: String },

    /// Indexing error.
    #[error("indexing error: {0}")]
    Indexing(String),
}

/// Server/API errors.
#[derive(Error, Debug)]
pub enum ServerError {
    /// Failed to bind to address.
    #[error("failed to bind to {address}: {reason}")]
    BindFailed { address: String, reason: String },

    /// Request handling error.
    #[error("request error: {0}")]
    Request(String),

    /// MCP protocol error.
    #[error("MCP error: {0}")]
    Mcp(String),
}

impl Error {
    /// Create a configuration error.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Create an internal error.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl StorageError {
    /// Create a not-found error.
    pub fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            id: id.into(),
        }
    }
}

#[cfg(test)]
mod tests;
