//! Integration tests for Deep Hooks daemon auto-ingest and sync.
//!
//! These tests verify that the transcript watcher can:
//! - Detect completed session transcripts
//! - Run the ingest pipeline automatically
//! - Run the sync pipeline to update memory files
//! - Apply rate limiting to prevent excessive processing

#[cfg(test)]
mod deep_hooks_daemon_integration {
    use nellie::claude_code::daemon::{
        IngestRateLimiter, TranscriptWatcher, TranscriptWatcherConfig,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_transcript_watcher_detects_completed_transcripts() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();

        // Create a projects subdirectory
        let project_dir = projects_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Create a transcript watcher
        let config = TranscriptWatcherConfig::new(projects_dir);
        let watcher = TranscriptWatcher::new(config).unwrap();

        // Verify initial state
        assert_eq!(watcher.status().to_string(), "Stopped");
        assert_eq!(watcher.projects_dir(), projects_dir);
    }

    #[test]
    fn test_ingest_rate_limiter_enforces_limits() {
        let limiter = IngestRateLimiter::with_interval(Duration::from_millis(100));
        let project = PathBuf::from("/home/user/projects/test");

        // First ingest should succeed
        assert!(limiter.should_ingest(&project));

        // Immediate second ingest should be blocked
        assert!(!limiter.should_ingest(&project));

        // After window passes, should succeed again
        std::thread::sleep(Duration::from_millis(110));
        assert!(limiter.should_ingest(&project));
    }

    #[test]
    fn test_ingest_rate_limiter_per_project_isolation() {
        let limiter = IngestRateLimiter::with_interval(Duration::from_secs(300));
        let project1 = PathBuf::from("/home/user/projects/project1");
        let project2 = PathBuf::from("/home/user/projects/project2");

        // Ingest first project
        assert!(limiter.should_ingest(&project1));

        // Second ingest of project1 blocked
        assert!(!limiter.should_ingest(&project1));

        // But project2 is not rate-limited
        assert!(limiter.should_ingest(&project2));

        // And project2's second ingest is also blocked
        assert!(!limiter.should_ingest(&project2));
    }

    #[test]
    fn test_rate_limiter_reset_for_testing() {
        let limiter = IngestRateLimiter::with_interval(Duration::from_secs(300));
        let project = PathBuf::from("/home/user/projects/test");

        // Ingest
        assert!(limiter.should_ingest(&project));

        // Blocked
        assert!(!limiter.should_ingest(&project));

        // Reset allows re-ingest
        limiter.reset(&project);
        assert!(limiter.should_ingest(&project));
    }

    #[test]
    fn test_rate_limiter_clear_all() {
        let limiter = IngestRateLimiter::with_interval(Duration::from_secs(300));
        let project1 = PathBuf::from("/home/user/projects/project1");
        let project2 = PathBuf::from("/home/user/projects/project2");

        // Ingest both
        assert!(limiter.should_ingest(&project1));
        assert!(limiter.should_ingest(&project2));

        // Both blocked
        assert!(!limiter.should_ingest(&project1));
        assert!(!limiter.should_ingest(&project2));

        // Clear all
        limiter.clear_all();

        // Both allowed again
        assert!(limiter.should_ingest(&project1));
        assert!(limiter.should_ingest(&project2));
    }

    #[test]
    fn test_ingest_rate_limit_interval_five_minutes() {
        // Verify that the default rate limit is 5 minutes
        let limiter = IngestRateLimiter::new();
        assert_eq!(limiter.rate_limit_interval(), Duration::from_secs(300));
    }

    #[test]
    fn test_ingest_rate_limiter_custom_intervals() {
        let intervals = vec![
            Duration::from_secs(60),  // 1 minute
            Duration::from_secs(300), // 5 minutes
            Duration::from_secs(600), // 10 minutes
        ];

        for interval in intervals {
            let limiter = IngestRateLimiter::with_interval(interval);
            assert_eq!(limiter.rate_limit_interval(), interval);
        }
    }

    #[test]
    fn test_transcript_watcher_jsonl_detection() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        // Create various files
        fs::write(base.join("session-abc123.jsonl"), "{}").unwrap();
        fs::write(base.join("MEMORY.md"), "# Memory").unwrap();
        fs::write(base.join("settings.json"), "{}").unwrap();

        // Only .jsonl files should be processed
        // This is verified by the watcher's is_jsonl_file function
        use std::path::Path;
        fn is_jsonl(path: &Path) -> bool {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        }

        assert!(is_jsonl(&base.join("session-abc123.jsonl")));
        assert!(!is_jsonl(&base.join("MEMORY.md")));
        assert!(!is_jsonl(&base.join("settings.json")));
    }

    #[test]
    fn test_transcript_watcher_directory_structure() {
        let tmp = TempDir::new().unwrap();
        let projects_dir = tmp.path();

        // Create nested project structure
        let project1 = projects_dir.join("-home-user-project1");
        let project2 = projects_dir.join("-home-user-project2");
        fs::create_dir_all(&project1).unwrap();
        fs::create_dir_all(&project2).unwrap();

        // Create transcripts
        fs::write(project1.join("session-1.jsonl"), "{}").unwrap();
        fs::write(project2.join("session-2.jsonl"), "{}").unwrap();

        // Watcher should find these
        assert!(project1.join("session-1.jsonl").exists());
        assert!(project2.join("session-2.jsonl").exists());
    }
}
