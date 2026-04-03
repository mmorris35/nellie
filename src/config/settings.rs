//! Configuration settings and validation.

use crate::{Error, Result};
use std::path::PathBuf;

/// Main configuration for Nellie server.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory for `SQLite` database and other data.
    pub data_dir: PathBuf,

    /// Host address to bind to.
    pub host: String,

    /// Port to listen on.
    pub port: u16,

    /// Log level (trace, debug, info, warn, error).
    pub log_level: String,

    /// Directories to watch for code changes.
    pub watch_dirs: Vec<PathBuf>,

    /// Maximum number of embedding worker threads.
    pub embedding_threads: usize,

    /// API key for authentication. If None, authentication is disabled (dev mode).
    pub api_key: Option<String>,

    /// Enable structural code analysis with Tree-sitter.
    pub enable_structural: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: dirs::data_local_dir()
                .map_or_else(|| PathBuf::from("./data"), |d| d.join("nellie")),
            host: "127.0.0.1".to_string(),
            port: 8080,
            log_level: "info".to_string(),
            watch_dirs: Vec::new(),
            embedding_threads: std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4),
            api_key: std::env::var("NELLIE_API_KEY").ok(),
            enable_structural: false,
        }
    }
}

impl Config {
    /// Create a new configuration with defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load configuration from environment variables and defaults.
    ///
    /// Note: This is a simplified loader. Full loading is done via clap in main.rs.
    ///
    /// # Errors
    ///
    /// Returns an error if configuration is invalid.
    pub fn load() -> Result<Self> {
        let config = Self::default();
        config.validate()?;
        Ok(config)
    }

    /// Validate configuration values.
    ///
    /// # Errors
    ///
    /// Returns an error if any configuration value is invalid.
    pub fn validate(&self) -> Result<()> {
        // Validate port
        if self.port == 0 {
            return Err(Error::config("port cannot be 0"));
        }

        // Validate log level
        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.log_level.to_lowercase().as_str()) {
            return Err(Error::config(format!(
                "invalid log level '{}', must be one of: {}",
                self.log_level,
                valid_levels.join(", ")
            )));
        }

        // Validate embedding threads
        if self.embedding_threads == 0 {
            return Err(Error::config("embedding_threads cannot be 0"));
        }

        if self.embedding_threads > 32 {
            return Err(Error::config(
                "embedding_threads cannot exceed 32 (hardware limit)",
            ));
        }

        // Validate host is not empty
        if self.host.is_empty() {
            return Err(Error::config("host cannot be empty"));
        }

        Ok(())
    }

    /// Get the path to the `SQLite` database file.
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("nellie.db")
    }

    /// Get the server address as a string.
    #[must_use]
    pub fn server_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.port, 8080);
        assert_eq!(config.host, "127.0.0.1");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_new() {
        let config = Config::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_invalid_port() {
        let config = Config {
            port: 0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("port"));
    }

    #[test]
    fn test_validate_invalid_log_level() {
        let config = Config {
            log_level: "invalid".to_string(),
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("log level"));
    }

    #[test]
    fn test_validate_invalid_embedding_threads_zero() {
        let config = Config {
            embedding_threads: 0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("embedding_threads"));
    }

    #[test]
    fn test_validate_invalid_embedding_threads_too_high() {
        let config = Config {
            embedding_threads: 100,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("32"));
    }

    #[test]
    fn test_validate_empty_host() {
        let config = Config {
            host: String::new(),
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host"));
    }

    #[test]
    fn test_database_path() {
        let config = Config {
            data_dir: PathBuf::from("/var/lib/nellie"),
            ..Default::default()
        };
        assert_eq!(
            config.database_path(),
            PathBuf::from("/var/lib/nellie/nellie.db")
        );
    }

    #[test]
    fn test_server_addr() {
        let config = Config {
            host: "0.0.0.0".to_string(),
            port: 9090,
            ..Default::default()
        };
        assert_eq!(config.server_addr(), "0.0.0.0:9090");
    }

    #[test]
    fn test_all_log_levels_valid() {
        for level in ["trace", "debug", "info", "warn", "error"] {
            let config = Config {
                log_level: level.to_string(),
                ..Default::default()
            };
            assert!(config.validate().is_ok(), "Level '{level}' should be valid");
        }
    }

    #[test]
    fn test_log_level_case_insensitive() {
        for level in ["TRACE", "Debug", "INFO", "Warn", "ERROR"] {
            let config = Config {
                log_level: level.to_string(),
                ..Default::default()
            };
            assert!(
                config.validate().is_ok(),
                "Level '{level}' should be valid (case insensitive)"
            );
        }
    }

    #[test]
    fn test_config_with_api_key() {
        let config = Config {
            api_key: Some("secret-key".to_string()),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
        assert_eq!(config.api_key, Some("secret-key".to_string()));
    }

    #[test]
    fn test_config_without_api_key() {
        let config = Config {
            api_key: None,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
        assert_eq!(config.api_key, None);
    }
}

/// Configuration for Nellie-V graph memory layer.
///
/// Controls graph-based knowledge memory behavior.
/// All graph functionality is gated behind `enabled` (default: false).
#[derive(Debug, Clone)]
pub struct GraphConfig {
    /// Enable graph memory (default: false)
    pub enabled: bool,
    /// Maximum number of graph nodes in memory
    pub max_nodes: usize,
    /// Confidence half-life in days for edge decay
    pub decay_half_life_days: f32,
    /// Minimum confidence before garbage collection
    pub gc_min_confidence: f32,
    /// Days before orphaned nodes are removed
    pub gc_orphan_days: u32,
    /// Starting confidence for new (provisional) edges
    pub provisional_threshold: f32,
    /// Success count needed to confirm a provisional edge
    pub confirmation_count: u32,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_nodes: 100_000,
            decay_half_life_days: 30.0,
            gc_min_confidence: 0.05,
            gc_orphan_days: 7,
            provisional_threshold: 0.3,
            confirmation_count: 2,
        }
    }
}

#[cfg(test)]
mod tests_graph {
    use super::*;

    #[test]
    fn test_graph_config_default() {
        let gc = GraphConfig::default();
        assert!(!gc.enabled);
        assert_eq!(gc.max_nodes, 100_000);
        assert!((gc.decay_half_life_days - 30.0).abs() < f32::EPSILON);
        assert!((gc.gc_min_confidence - 0.05).abs() < f32::EPSILON);
        assert_eq!(gc.gc_orphan_days, 7);
        assert!((gc.provisional_threshold - 0.3).abs() < f32::EPSILON);
        assert_eq!(gc.confirmation_count, 2);
    }
}
