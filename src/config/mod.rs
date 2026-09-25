//! Configuration management for Nellie.
//!
//! Supports configuration from:
//! - Command-line arguments (highest priority)
//! - Environment variables
//! - Configuration file (lowest priority)

mod settings;

pub use settings::{
    expand_tilde, Config, FileConfig, FileDataSection, FileDeepHooksSection, FileServerSection,
    FileToggleSection, FileWatchSection, GraphConfig,
};
