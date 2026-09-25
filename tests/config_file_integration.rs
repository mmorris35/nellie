//! Integration tests for the YAML config-file loader (Phase 1, Gap 1).
//!
//! Verifies that a `config.yaml` shaped like `config.example.yaml` parses and
//! that `watch.paths` becomes the watch-dir list used to enable the watcher.

use nellie::config::FileConfig;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_load_example_shaped_config() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.yaml");
    fs::write(
        &path,
        r#"
server:
  host: "127.0.0.1"
  port: 8765
data:
  dir: "~/.local/share/nellie"
watch:
  paths:
    - "/home/user/projects/app-one"
    - "/home/user/github/app-two"
graph:
  enabled: true
structural:
  enabled: true
deep_hooks:
  enabled: false
  sync_interval: 45
"#,
    )
    .unwrap();

    let cfg = FileConfig::load(&path).unwrap();
    assert_eq!(cfg.server.host.as_deref(), Some("127.0.0.1"));
    assert_eq!(cfg.server.port, Some(8765));
    assert_eq!(
        cfg.watch_paths(),
        vec![
            PathBuf::from("/home/user/projects/app-one"),
            PathBuf::from("/home/user/github/app-two"),
        ]
    );
    assert_eq!(cfg.graph.enabled, Some(true));
    assert_eq!(cfg.structural.enabled, Some(true));
    assert_eq!(cfg.deep_hooks.enabled, Some(false));
    assert_eq!(cfg.deep_hooks.sync_interval, Some(45));
}

#[test]
fn test_default_lookup_finds_data_dir_config() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("config.yaml");
    fs::write(&cfg_path, "watch:\n  paths:\n    - \"/srv/repo\"\n").unwrap();

    let found = FileConfig::find_default(tmp.path()).expect("should find config.yaml");
    assert_eq!(found, cfg_path);

    let cfg = FileConfig::load(&found).unwrap();
    assert_eq!(cfg.watch_paths(), vec![PathBuf::from("/srv/repo")]);
}

#[test]
fn test_watch_only_config_is_valid() {
    // The minimal config that makes the watcher reachable: just watch.paths.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("nellie.yaml");
    fs::write(&path, "watch:\n  paths:\n    - \"/code/here\"\n").unwrap();

    let cfg = FileConfig::load(&path).unwrap();
    assert_eq!(cfg.watch_paths(), vec![PathBuf::from("/code/here")]);
    // Everything else falls back to defaults (None) so CLI/env still win.
    assert!(cfg.server.host.is_none());
    assert!(cfg.server.port.is_none());
    assert!(cfg.graph.enabled.is_none());
}

#[test]
fn test_malformed_config_errors_clearly() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.yaml");
    fs::write(&path, "watch:\n  paths: \"not-a-list\"\n").unwrap();

    let err = FileConfig::load(&path).unwrap_err();
    assert!(
        err.to_string().contains("cannot parse config file"),
        "expected parse error, got: {err}"
    );
}
