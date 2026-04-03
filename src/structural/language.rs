//! Language detection and Tree-sitter grammar registry.
//!
//! Maps file extensions to Tree-sitter grammars for structural parsing.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Languages supported by the structural parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedLanguage {
    Python,
    TypeScript,
    JavaScript,
    Rust,
    Go,
}

impl SupportedLanguage {
    /// Detect language from a file extension string (without the leading dot).
    ///
    /// Returns `None` for unsupported extensions.
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "py" => Some(Self::Python),
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" => Some(Self::JavaScript),
            "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    /// Detect language from a file path by examining the extension.
    ///
    /// Returns `None` for unsupported files or files without extensions.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        Self::from_extension(ext)
    }

    /// Get the Tree-sitter `Language` grammar for this language.
    #[must_use]
    pub fn tree_sitter_language(&self) -> tree_sitter::Language {
        match self {
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    /// Get the lowercase name string for this language.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
            Self::Go => "go",
        }
    }

    /// String representation for SQLite storage.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.name()
    }

    /// Parse from string (SQLite storage format).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "python" => Some(Self::Python),
            "typescript" => Some(Self::TypeScript),
            "javascript" => Some(Self::JavaScript),
            "rust" => Some(Self::Rust),
            "go" => Some(Self::Go),
            _ => None,
        }
    }
}

impl fmt::Display for SupportedLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_from_extension_python() {
        assert_eq!(
            SupportedLanguage::from_extension("py"),
            Some(SupportedLanguage::Python)
        );
    }

    #[test]
    fn test_from_extension_typescript() {
        assert_eq!(
            SupportedLanguage::from_extension("ts"),
            Some(SupportedLanguage::TypeScript)
        );
        assert_eq!(
            SupportedLanguage::from_extension("tsx"),
            Some(SupportedLanguage::TypeScript)
        );
    }

    #[test]
    fn test_from_extension_javascript() {
        assert_eq!(
            SupportedLanguage::from_extension("js"),
            Some(SupportedLanguage::JavaScript)
        );
        assert_eq!(
            SupportedLanguage::from_extension("jsx"),
            Some(SupportedLanguage::JavaScript)
        );
    }

    #[test]
    fn test_from_extension_rust() {
        assert_eq!(
            SupportedLanguage::from_extension("rs"),
            Some(SupportedLanguage::Rust)
        );
    }

    #[test]
    fn test_from_extension_go() {
        assert_eq!(
            SupportedLanguage::from_extension("go"),
            Some(SupportedLanguage::Go)
        );
    }

    #[test]
    fn test_from_extension_unsupported() {
        assert_eq!(SupportedLanguage::from_extension("xyz"), None);
        assert_eq!(SupportedLanguage::from_extension("c"), None);
        assert_eq!(SupportedLanguage::from_extension("java"), None);
        assert_eq!(SupportedLanguage::from_extension(""), None);
    }

    #[test]
    fn test_from_path() {
        assert_eq!(
            SupportedLanguage::from_path(&PathBuf::from("main.py")),
            Some(SupportedLanguage::Python)
        );
        assert_eq!(
            SupportedLanguage::from_path(&PathBuf::from("src/lib.rs")),
            Some(SupportedLanguage::Rust)
        );
        assert_eq!(
            SupportedLanguage::from_path(&PathBuf::from("app.tsx")),
            Some(SupportedLanguage::TypeScript)
        );
        assert_eq!(
            SupportedLanguage::from_path(&PathBuf::from("index.jsx")),
            Some(SupportedLanguage::JavaScript)
        );
        assert_eq!(
            SupportedLanguage::from_path(&PathBuf::from("main.go")),
            Some(SupportedLanguage::Go)
        );
    }

    #[test]
    fn test_from_path_no_extension() {
        assert_eq!(
            SupportedLanguage::from_path(&PathBuf::from("Makefile")),
            None
        );
    }

    #[test]
    fn test_from_path_unsupported_extension() {
        assert_eq!(
            SupportedLanguage::from_path(&PathBuf::from("style.css")),
            None
        );
    }

    #[test]
    fn test_tree_sitter_language_loads() {
        // Verify each grammar can be loaded without panic
        let languages = [
            SupportedLanguage::Python,
            SupportedLanguage::TypeScript,
            SupportedLanguage::JavaScript,
            SupportedLanguage::Rust,
            SupportedLanguage::Go,
        ];
        for lang in languages {
            let _ts_lang = lang.tree_sitter_language();
        }
    }

    #[test]
    fn test_name() {
        assert_eq!(SupportedLanguage::Python.name(), "python");
        assert_eq!(SupportedLanguage::TypeScript.name(), "typescript");
        assert_eq!(SupportedLanguage::JavaScript.name(), "javascript");
        assert_eq!(SupportedLanguage::Rust.name(), "rust");
        assert_eq!(SupportedLanguage::Go.name(), "go");
    }

    #[test]
    fn test_roundtrip_parse() {
        for lang in [
            SupportedLanguage::Python,
            SupportedLanguage::TypeScript,
            SupportedLanguage::JavaScript,
            SupportedLanguage::Rust,
            SupportedLanguage::Go,
        ] {
            assert_eq!(SupportedLanguage::parse(lang.as_str()), Some(lang));
        }
        assert_eq!(SupportedLanguage::parse("unknown"), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", SupportedLanguage::Python), "python");
        assert_eq!(format!("{}", SupportedLanguage::Rust), "rust");
    }
}
