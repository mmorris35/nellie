//! File filtering with gitignore support.

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::Result;

/// Supported code file extensions and their languages.
const CODE_EXTENSIONS: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("py", "python"),
    ("js", "javascript"),
    ("ts", "typescript"),
    ("jsx", "javascript"),
    ("tsx", "typescript"),
    ("go", "go"),
    ("java", "java"),
    ("c", "c"),
    ("cpp", "cpp"),
    ("cc", "cpp"),
    ("h", "c"),
    ("hpp", "cpp"),
    ("cs", "csharp"),
    ("rb", "ruby"),
    ("php", "php"),
    ("swift", "swift"),
    ("kt", "kotlin"),
    ("scala", "scala"),
    ("sh", "shell"),
    ("bash", "shell"),
    ("zsh", "shell"),
    ("sql", "sql"),
    ("md", "markdown"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("json", "json"),
    ("toml", "toml"),
    ("xml", "xml"),
    ("html", "html"),
    ("css", "css"),
    ("scss", "scss"),
    ("vue", "vue"),
    ("svelte", "svelte"),
];

/// Maximum file size to index (default 1MB). Files larger than this are skipped.
const MAX_FILE_SIZE: u64 = 1_048_576;

/// File filter for indexing.
#[derive(Debug)]
pub struct FileFilter {
    gitignore: Option<Gitignore>,
    #[allow(dead_code)]
    base_path: std::path::PathBuf,
    max_file_size: u64,
}

impl FileFilter {
    /// Create a new file filter.
    ///
    /// If a `.gitignore` exists in `base_path`, it will be used for filtering.
    pub fn new(base_path: impl AsRef<Path>) -> Self {
        let base_path = base_path.as_ref().to_path_buf();
        let gitignore_path = base_path.join(".gitignore");

        let gitignore = if gitignore_path.exists() {
            let mut builder = GitignoreBuilder::new(&base_path);
            if builder.add(&gitignore_path).is_none() {
                builder.build().ok()
            } else {
                None
            }
        } else {
            None
        };

        Self {
            gitignore,
            base_path,
            max_file_size: MAX_FILE_SIZE,
        }
    }

    /// Create a filter with custom ignore patterns.
    ///
    /// # Errors
    ///
    /// Returns an error if patterns are invalid.
    pub fn with_patterns(base_path: impl AsRef<Path>, patterns: &[&str]) -> Result<Self> {
        let base_path = base_path.as_ref().to_path_buf();
        let mut builder = GitignoreBuilder::new(&base_path);

        for pattern in patterns {
            builder
                .add_line(None, pattern)
                .map_err(|e| crate::Error::config(format!("invalid pattern: {e}")))?;
        }

        let gitignore = builder
            .build()
            .map_err(|e| crate::Error::config(format!("failed to build gitignore: {e}")))?;

        Ok(Self {
            gitignore: Some(gitignore),
            base_path,
            max_file_size: MAX_FILE_SIZE,
        })
    }

    /// Check if a file should be indexed.
    #[must_use]
    pub fn should_index(&self, path: &Path) -> bool {
        // Must be a file
        if !path.is_file() {
            return false;
        }

        // Must be a code file
        if !Self::is_code_file(path) {
            return false;
        }

        // Must not exceed max file size
        if let Ok(metadata) = path.metadata() {
            if metadata.len() > self.max_file_size {
                return false;
            }
        }

        // Must not be ignored
        if let Some(ref gi) = self.gitignore {
            if gi.matched(path, false).is_ignore() {
                return false;
            }
        }

        // Default ignores (check relative to base_path to avoid false positives
        // from system temp dirs like /tmp/.tmpXXXXXX)
        if Self::is_default_ignored_relative(path, Some(&self.base_path)) {
            return false;
        }

        true
    }

    /// Check if a path is a code file based on extension.
    #[must_use]
    pub fn is_code_file(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                CODE_EXTENSIONS
                    .iter()
                    .any(|(e, _)| *e == ext.to_lowercase())
            })
    }

    /// Get the language for a file based on extension.
    #[must_use]
    pub fn detect_language(path: &Path) -> Option<&'static str> {
        path.extension().and_then(|e| e.to_str()).and_then(|ext| {
            CODE_EXTENSIONS
                .iter()
                .find(|(e, _)| *e == ext.to_lowercase())
                .map(|(_, lang)| *lang)
        })
    }

    /// Check if a path matches default ignore patterns.
    ///
    /// Only checks path components relative to a base path (if provided)
    /// to avoid false positives from system temp directory names.
    #[cfg(test)]
    fn is_default_ignored(path: &Path) -> bool {
        Self::is_default_ignored_relative(path, None)
    }

    /// Check if a path matches default ignore patterns, optionally relative to a base.
    fn is_default_ignored_relative(path: &Path, base: Option<&Path>) -> bool {
        // If a base is provided, only check the relative portion
        let check_path = base.map_or(path, |base| path.strip_prefix(base).unwrap_or(path));

        let path_str = check_path.to_string_lossy();

        // Dotdir heuristic: any path component starting with '.' is likely junk
        // (e.g., .mypy_cache, .pytest_cache, .tox, .cache, .next, .nuxt, .terraform)
        // Exception: .github (workflows, actions, etc.)
        for component in path_str.split('/') {
            if component.starts_with('.')
                && component.len() > 1
                && component != ".github"
                && component != ".gitignore"
            {
                return true;
            }
        }

        // Non-dot directories to ignore
        let ignored_dirs = [
            "/node_modules/",
            "/target/",
            "/build/",
            "/dist/",
            "/__pycache__/",
            "/venv/",
            "/vendor/",
            "/obj/",
            "/bin/",
            "/coverage/",
            "/egg-info/",
            "/Raw_Evidence/",
            "/RawEvidence/",
            "/Evidence_",
            "/.terraform/",
            "/bower_components/",
            "/.venv/",
            "/site-packages/",
        ];

        for dir in ignored_dirs {
            if path_str.contains(dir) {
                return true;
            }
        }

        // Common files to ignore
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let ignored_files = [".DS_Store", "Thumbs.db", ".env", ".env.local"];
            if ignored_files.contains(&name) {
                return true;
            }

            // Ignore lock files
            let name_path = std::path::Path::new(name);
            let lower = name.to_lowercase();
            if name_path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
                || lower.ends_with("-lock.json")
                || lower == "package-lock.json"
            {
                return true;
            }

            // Ignore minified files
            if lower.ends_with(".min.js") || lower.ends_with(".min.css") {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_is_code_file() {
        assert!(FileFilter::is_code_file(Path::new("main.rs")));
        assert!(FileFilter::is_code_file(Path::new("app.py")));
        assert!(FileFilter::is_code_file(Path::new("index.tsx")));
        assert!(!FileFilter::is_code_file(Path::new("image.png")));
        assert!(!FileFilter::is_code_file(Path::new("document.pdf")));
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(
            FileFilter::detect_language(Path::new("main.rs")),
            Some("rust")
        );
        assert_eq!(
            FileFilter::detect_language(Path::new("app.py")),
            Some("python")
        );
        assert_eq!(
            FileFilter::detect_language(Path::new("index.tsx")),
            Some("typescript")
        );
        assert_eq!(FileFilter::detect_language(Path::new("unknown.xyz")), None);
    }

    #[test]
    fn test_default_ignored() {
        assert!(FileFilter::is_default_ignored(Path::new(
            "/project/node_modules/pkg/index.js"
        )));
        assert!(FileFilter::is_default_ignored(Path::new(
            "/project/.git/config"
        )));
        assert!(FileFilter::is_default_ignored(Path::new(
            "/project/target/debug/main"
        )));
        assert!(FileFilter::is_default_ignored(Path::new("/project/.env")));
        assert!(!FileFilter::is_default_ignored(Path::new(
            "/project/src/main.rs"
        )));
    }

    #[test]
    fn test_filter_with_gitignore() {
        let tmp = TempDir::new().unwrap();

        // Create .gitignore
        fs::write(tmp.path().join(".gitignore"), "*.log\ntest_output/\n").unwrap();

        // Create test files
        fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("debug.log"), "log content").unwrap();

        let filter = FileFilter::new(tmp.path());

        assert!(filter.should_index(&tmp.path().join("main.rs")));
        assert!(!filter.should_index(&tmp.path().join("debug.log")));
    }

    #[test]
    fn test_max_file_size() {
        let tmp = TempDir::new().unwrap();

        // Small file - should be indexed
        fs::write(tmp.path().join("small.rs"), "fn main() {}").unwrap();

        // Large file - should be skipped
        let large_content = "x".repeat(2_000_000); // 2MB
        fs::write(tmp.path().join("huge.rs"), large_content).unwrap();

        let filter = FileFilter::new(tmp.path());

        assert!(filter.should_index(&tmp.path().join("small.rs")));
        assert!(!filter.should_index(&tmp.path().join("huge.rs")));
    }

    #[test]
    fn test_evidence_dirs_ignored() {
        assert!(FileFilter::is_default_ignored(Path::new(
            "/project/MSTRG/Evidence_20251002/RawEvidence.json"
        )));
        assert!(FileFilter::is_default_ignored(Path::new(
            "/project/Raw_Evidence/AC_Evidence.json"
        )));
    }

    #[test]
    fn test_filter_with_patterns() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("test.rs"), "fn test() {}").unwrap();

        let filter = FileFilter::with_patterns(tmp.path(), &["test*.rs"]).unwrap();

        assert!(filter.should_index(&tmp.path().join("main.rs")));
        assert!(!filter.should_index(&tmp.path().join("test.rs")));
    }
}
