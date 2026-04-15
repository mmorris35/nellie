//! Nellie Production Library
//!
//! Production-grade semantic code memory system for enterprise engineering teams.
//!
//! # Architecture
//!
//! Nellie is organized into the following modules:
//!
//! - [`claude_code`]: Direct integration with Claude Code's native file-based systems
//! - [`config`]: Configuration management (CLI args, environment, files)
//! - [`error`]: Error types and Result aliases
//! - [`storage`]: `SQLite` database with `sqlite-vec` for vector search
//! - [`embeddings`]: ONNX-based embedding generation
//! - [`watcher`]: File system watching and indexing
//! - [`server`]: MCP and REST API servers
//!
//! # Example
//!
//! ```rust,ignore
//! use nellie::config::Config;
//! use nellie::server::Server;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = Config::load()?;
//!     let server = Server::new(config).await?;
//!     server.run().await
//! }
//! ```

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
// Allow noisy pedantic lints project-wide
#![allow(clippy::doc_markdown)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::similar_names)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_lossless)]

pub mod bootstrap;
pub mod claude_code;
pub mod config;
pub mod embeddings;
pub mod error;
pub mod graph;
pub mod server;
pub mod setup;
pub mod storage;
pub mod structural;
pub mod watcher;

pub use config::Config;
pub use error::{Error, Result};
