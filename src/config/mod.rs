//! Configuration management for Nellie.
//!
//! Supports configuration from:
//! - Command-line arguments (highest priority)
//! - Environment variables
//! - Configuration file (lowest priority)

mod settings;

pub use settings::{Config, GraphConfig};
