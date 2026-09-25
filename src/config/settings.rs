//! Configuration settings and validation.

use crate::{Error, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
            embedding_threads: std::thread::available_parallelism().map_or(4, |n| n.get().min(4)),
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

// ---------------------------------------------------------------------------
// File-based configuration (config.yaml)
// ---------------------------------------------------------------------------

/// Configuration loaded from a YAML file (`config.yaml` / `nellie.yaml`).
///
/// Mirrors the shape of `config.example.yaml`. All sections and fields are
/// optional so a partial file (e.g. only `watch.paths`) is valid.
///
/// Precedence, resolved in `main.rs`: **CLI flag > environment variable >
/// config file > built-in default.** The file only fills in values that were
/// not supplied on the command line or via the environment.
///
/// Note: `data.dir` is parsed but NOT applied — the config file itself is
/// located relative to the data directory, so the data dir must come from
/// CLI/env/default to avoid a circular lookup.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    /// `server:` section (host, port).
    pub server: FileServerSection,
    /// `data:` section (dir) — parsed but not applied (see struct docs).
    pub data: FileDataSection,
    /// `watch:` section (paths).
    pub watch: FileWatchSection,
    /// `graph:` section (enabled).
    pub graph: FileToggleSection,
    /// `structural:` section (enabled).
    pub structural: FileToggleSection,
    /// `deep_hooks:` section (enabled, sync_interval).
    pub deep_hooks: FileDeepHooksSection,
}

/// `server:` section of the config file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileServerSection {
    /// Bind address for the HTTP server.
    pub host: Option<String>,
    /// Port for the HTTP server.
    pub port: Option<u16>,
}

/// `data:` section of the config file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileDataSection {
    /// Data directory (parsed for forward-compat; not applied — see [`FileConfig`]).
    pub dir: Option<String>,
}

/// `watch:` section of the config file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileWatchSection {
    /// Directories to watch for code changes.
    pub paths: Vec<String>,
}

/// Generic `enabled:` toggle section (`graph:`, `structural:`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileToggleSection {
    /// Whether the feature is enabled.
    pub enabled: Option<bool>,
}

/// `deep_hooks:` section of the config file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileDeepHooksSection {
    /// Whether the Deep Hooks daemon is enabled.
    pub enabled: Option<bool>,
    /// Periodic sync interval in minutes.
    pub sync_interval: Option<u64>,
}

impl FileConfig {
    /// Load and parse a YAML config file.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if the file cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            Error::config(format!("cannot read config file {}: {e}", path.display()))
        })?;
        serde_yaml::from_str(&raw)
            .map_err(|e| Error::config(format!("cannot parse config file {}: {e}", path.display())))
    }

    /// Find the default config file location.
    ///
    /// Looks for `<data_dir>/config.yaml`, then `./nellie.yaml`.
    /// Returns `None` if neither exists.
    #[must_use]
    pub fn find_default(data_dir: &Path) -> Option<PathBuf> {
        let candidates = [data_dir.join("config.yaml"), PathBuf::from("nellie.yaml")];
        candidates.into_iter().find(|p| p.is_file())
    }

    /// Watch paths from the file with `~` expanded to the home directory.
    #[must_use]
    pub fn watch_paths(&self) -> Vec<PathBuf> {
        self.watch.paths.iter().map(|p| expand_tilde(p)).collect()
    }
}

/// Expand a leading `~` or `~/` to the user's home directory.
///
/// Returns the path unchanged if it does not start with `~` or if the home
/// directory cannot be determined.
#[must_use]
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests_file_config {
    use super::*;

    #[test]
    fn test_parse_full_example_shape() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 9999
data:
  dir: "~/.local/share/nellie"
watch:
  paths:
    - "/home/user/projects/my-app"
graph:
  enabled: true
structural:
  enabled: false
deep_hooks:
  enabled: true
  sync_interval: 15
"#;
        let cfg: FileConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.server.host.as_deref(), Some("0.0.0.0"));
        assert_eq!(cfg.server.port, Some(9999));
        assert_eq!(cfg.watch.paths, vec!["/home/user/projects/my-app"]);
        assert_eq!(cfg.graph.enabled, Some(true));
        assert_eq!(cfg.structural.enabled, Some(false));
        assert_eq!(cfg.deep_hooks.enabled, Some(true));
        assert_eq!(cfg.deep_hooks.sync_interval, Some(15));
    }

    #[test]
    fn test_parse_watch_only() {
        let yaml = "watch:\n  paths:\n    - \"/tmp/repo\"\n";
        let cfg: FileConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.watch_paths(), vec![PathBuf::from("/tmp/repo")]);
        assert!(cfg.server.host.is_none());
        assert!(cfg.server.port.is_none());
        assert!(cfg.graph.enabled.is_none());
    }

    #[test]
    fn test_parse_empty_file() {
        let cfg: FileConfig = serde_yaml::from_str("{}").unwrap();
        assert!(cfg.watch.paths.is_empty());
        assert!(cfg.server.host.is_none());
    }

    #[test]
    fn test_parse_unknown_keys_ignored() {
        // config.example.yaml ships commented-out/extra sections;
        // unknown keys must not be a hard error.
        let yaml = "watch:\n  paths: []\nallowed_hostname: \"box\"\n";
        let cfg: FileConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.watch.paths.is_empty());
    }

    #[test]
    fn test_load_missing_file_errors() {
        let err = FileConfig::load(Path::new("/nonexistent/config.yaml")).unwrap_err();
        assert!(err.to_string().contains("cannot read config file"));
    }

    #[test]
    fn test_load_invalid_yaml_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, "watch: [not: {a map").unwrap();
        let err = FileConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("cannot parse config file"));
    }

    #[test]
    fn test_load_valid_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, "watch:\n  paths:\n    - \"/srv/code\"\n").unwrap();
        let cfg = FileConfig::load(&path).unwrap();
        assert_eq!(cfg.watch_paths(), vec![PathBuf::from("/srv/code")]);
    }

    #[test]
    fn test_find_default_prefers_data_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.yaml");
        std::fs::write(&cfg_path, "watch:\n  paths: []\n").unwrap();
        assert_eq!(FileConfig::find_default(tmp.path()), Some(cfg_path));
    }

    #[test]
    fn test_find_default_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No config.yaml in data dir; ./nellie.yaml existence depends on CWD,
        // so only assert the data-dir candidate is skipped when absent.
        let found = FileConfig::find_default(tmp.path());
        if let Some(p) = found {
            assert_eq!(p, PathBuf::from("nellie.yaml"));
        }
    }

    #[test]
    fn test_expand_tilde() {
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde("~/code"), home.join("code"));
            assert_eq!(expand_tilde("~"), home);
        }
    }
}
