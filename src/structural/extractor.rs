//! Symbol extraction from Tree-sitter ASTs.
//!
//! Defines shared types for extracted symbols and the trait that all
//! language-specific extractors must implement.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::language::SupportedLanguage;

/// The kind of symbol extracted from source code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Import,
    CallSite,
    TestFunction,
}

impl SymbolKind {
    /// String representation for SQLite storage.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Import => "import",
            Self::CallSite => "call_site",
            Self::TestFunction => "test_function",
        }
    }

    /// Parse from string (SQLite storage format).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "function" => Some(Self::Function),
            "class" => Some(Self::Class),
            "method" => Some(Self::Method),
            "import" => Some(Self::Import),
            "call_site" => Some(Self::CallSite),
            "test_function" => Some(Self::TestFunction),
            _ => None,
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A symbol extracted from source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedSymbol {
    /// Symbol name (e.g., function name, class name, import path).
    pub name: String,
    /// What kind of symbol this is.
    pub kind: SymbolKind,
    /// Starting line (0-based).
    pub start_line: u32,
    /// Ending line (0-based, inclusive).
    pub end_line: u32,
    /// Parent scope (e.g., class name for a method, function name for a nested call).
    pub scope: Option<String>,
    /// Function/method signature if available.
    pub signature: Option<String>,
    /// Language name string.
    pub language: String,
}

/// Result of extracting symbols from a single file.
#[derive(Debug, Clone)]
pub struct ExtractionResult {
    /// All symbols found in the file.
    pub symbols: Vec<ExtractedSymbol>,
    /// Path of the file that was analyzed.
    pub file_path: PathBuf,
    /// Language detected for the file.
    pub language: SupportedLanguage,
}

/// Trait for language-specific symbol extractors.
///
/// Each supported language implements this trait to walk the Tree-sitter AST
/// and extract symbols using language-specific node kinds and patterns.
pub trait LanguageExtractor {
    /// Extract all symbols from a parsed Tree-sitter tree.
    ///
    /// `source` is the original source bytes (needed for `node.utf8_text()`).
    fn extract(&self, tree: &tree_sitter::Tree, source: &[u8]) -> Vec<ExtractedSymbol>;
}

/// Extract symbols from source bytes with a known language.
///
/// Creates a parser, parses the source, and dispatches to the appropriate
/// language-specific extractor.
///
/// # Errors
///
/// Returns an error if parsing fails.
pub fn extract_symbols(
    path: &std::path::Path,
    source: &[u8],
    language: SupportedLanguage,
) -> crate::Result<ExtractionResult> {
    let mut parser = super::parser::StructuralParser::new();
    let tree = parser.parse(source, language)?;

    let symbols: Vec<ExtractedSymbol> = match language {
        SupportedLanguage::Python => {
            super::extractors::python::PythonExtractor.extract(&tree, source)
        }
        SupportedLanguage::TypeScript => {
            super::extractors::typescript::TypeScriptExtractor::new("typescript")
                .extract(&tree, source)
        }
        SupportedLanguage::JavaScript => {
            super::extractors::typescript::TypeScriptExtractor::new("javascript")
                .extract(&tree, source)
        }
        SupportedLanguage::Rust => {
            super::extractors::rust_lang::RustExtractor.extract(&tree, source)
        }
        SupportedLanguage::Go => super::extractors::go::GoExtractor.extract(&tree, source),
    };

    Ok(ExtractionResult {
        symbols,
        file_path: path.to_path_buf(),
        language,
    })
}

/// Extract symbols from a file on disk.
///
/// Reads the file, detects language from extension, and extracts symbols.
/// Returns `Ok` with empty symbols for unsupported file extensions (not an error).
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsing fails.
pub fn extract_symbols_from_file(
    path: &std::path::Path,
) -> crate::Result<Option<ExtractionResult>> {
    let Some(language) = SupportedLanguage::from_path(path) else {
        return Ok(None);
    };

    let source = std::fs::read(path)?;
    let result = extract_symbols(path, &source, language)?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_kind_roundtrip() {
        for kind in [
            SymbolKind::Function,
            SymbolKind::Class,
            SymbolKind::Method,
            SymbolKind::Import,
            SymbolKind::CallSite,
            SymbolKind::TestFunction,
        ] {
            assert_eq!(SymbolKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(SymbolKind::parse("unknown"), None);
    }

    #[test]
    fn test_symbol_kind_display() {
        assert_eq!(format!("{}", SymbolKind::Function), "function");
        assert_eq!(format!("{}", SymbolKind::TestFunction), "test_function");
    }

    #[test]
    fn test_extract_symbols_python() {
        let source = b"def hello():\n    pass";
        let result = extract_symbols(
            std::path::Path::new("test.py"),
            source,
            SupportedLanguage::Python,
        )
        .unwrap();
        assert_eq!(result.language, SupportedLanguage::Python);
        assert!(!result.symbols.is_empty());
    }

    #[test]
    fn test_extract_symbols_rust() {
        let source = b"fn main() {}";
        let result = extract_symbols(
            std::path::Path::new("test.rs"),
            source,
            SupportedLanguage::Rust,
        )
        .unwrap();
        assert_eq!(result.language, SupportedLanguage::Rust);
        let funcs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
    }

    #[test]
    fn test_extract_symbols_typescript() {
        let source = b"function hello(): void {}";
        let result = extract_symbols(
            std::path::Path::new("test.ts"),
            source,
            SupportedLanguage::TypeScript,
        )
        .unwrap();
        assert!(!result.symbols.is_empty());
    }

    #[test]
    fn test_extract_symbols_javascript() {
        let source = b"function hello() {}";
        let result = extract_symbols(
            std::path::Path::new("test.js"),
            source,
            SupportedLanguage::JavaScript,
        )
        .unwrap();
        assert!(!result.symbols.is_empty());
    }

    #[test]
    fn test_extract_symbols_go() {
        let source = b"package main\nfunc main() {}";
        let result = extract_symbols(
            std::path::Path::new("test.go"),
            source,
            SupportedLanguage::Go,
        )
        .unwrap();
        assert!(!result.symbols.is_empty());
    }

    #[test]
    fn test_extract_symbols_from_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("test.py");
        std::fs::write(&path, "def hello():\n    pass").unwrap();
        let result = extract_symbols_from_file(&path).unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.language, SupportedLanguage::Python);
    }

    #[test]
    fn test_extract_symbols_from_file_unsupported() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("readme.md");
        std::fs::write(&path, "# Hello").unwrap();
        let result = extract_symbols_from_file(&path).unwrap();
        assert!(result.is_none());
    }
}
