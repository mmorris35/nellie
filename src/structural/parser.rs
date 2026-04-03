//! Core AST parser using Tree-sitter.
//!
//! Provides a reusable parser that can parse source code in any supported
//! language into a Tree-sitter syntax tree.

use std::path::Path;

use crate::error::StorageError;
use crate::Result;

use super::language::SupportedLanguage;

/// Structural parser wrapping a Tree-sitter `Parser` instance.
///
/// The parser is reusable across multiple files -- call `parse()` or
/// `parse_file()` repeatedly. The language grammar is set before each parse.
pub struct StructuralParser {
    parser: tree_sitter::Parser,
}

impl StructuralParser {
    /// Create a new structural parser.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: tree_sitter::Parser::new(),
        }
    }

    /// Parse source bytes with a specified language.
    ///
    /// # Errors
    ///
    /// Returns an error if the language grammar fails to load or parsing fails.
    pub fn parse(
        &mut self,
        source: &[u8],
        language: SupportedLanguage,
    ) -> Result<tree_sitter::Tree> {
        let ts_lang = language.tree_sitter_language();
        self.parser.set_language(&ts_lang).map_err(|e| {
            crate::Error::Internal(format!(
                "failed to set tree-sitter language for {}: {e}",
                language.name()
            ))
        })?;

        self.parser.parse(source, None).ok_or_else(|| {
            crate::Error::Internal(format!(
                "tree-sitter parse returned None for language {}",
                language.name()
            ))
        })
    }

    /// Parse a file from disk: reads it, detects language, returns the AST and language.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be read
    /// - The file extension is not a supported language
    /// - Parsing fails
    pub fn parse_file(
        &mut self,
        path: &Path,
    ) -> Result<(tree_sitter::Tree, SupportedLanguage, Vec<u8>)> {
        let language = SupportedLanguage::from_path(path).ok_or_else(|| {
            StorageError::Database(format!("unsupported file extension: {}", path.display()))
        })?;

        let source = std::fs::read(path)?;
        let tree = self.parse(&source, language)?;
        Ok((tree, language, source))
    }
}

impl Default for StructuralParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python() {
        let mut parser = StructuralParser::new();
        let source = b"def hello(): pass";
        let tree = parser.parse(source, SupportedLanguage::Python).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "module");
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_rust() {
        let mut parser = StructuralParser::new();
        let source = b"fn main() {}";
        let tree = parser.parse(source, SupportedLanguage::Rust).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_javascript() {
        let mut parser = StructuralParser::new();
        let source = b"function hello() {}";
        let tree = parser.parse(source, SupportedLanguage::JavaScript).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn test_parse_typescript() {
        let mut parser = StructuralParser::new();
        let source = b"function hello(): void {}";
        let tree = parser.parse(source, SupportedLanguage::TypeScript).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn test_parse_go() {
        let mut parser = StructuralParser::new();
        let source = b"package main\nfunc main() {}";
        let tree = parser.parse(source, SupportedLanguage::Go).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
    }

    #[test]
    fn test_parse_file_from_disk() {
        let mut parser = StructuralParser::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("test.py");
        std::fs::write(&file_path, "def foo(): pass").unwrap();

        let (tree, lang, source) = parser.parse_file(&file_path).unwrap();
        assert_eq!(lang, SupportedLanguage::Python);
        assert_eq!(tree.root_node().kind(), "module");
        assert!(!source.is_empty());
    }

    #[test]
    fn test_parse_file_unsupported_extension() {
        let mut parser = StructuralParser::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("test.xyz");
        std::fs::write(&file_path, "some content").unwrap();

        let result = parser.parse_file(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_parser_reuse_across_languages() {
        let mut parser = StructuralParser::new();

        let tree1 = parser
            .parse(b"def hello(): pass", SupportedLanguage::Python)
            .unwrap();
        assert_eq!(tree1.root_node().kind(), "module");

        let tree2 = parser
            .parse(b"fn main() {}", SupportedLanguage::Rust)
            .unwrap();
        assert_eq!(tree2.root_node().kind(), "source_file");

        // Parse Python again to verify reuse
        let tree3 = parser
            .parse(b"class Foo: pass", SupportedLanguage::Python)
            .unwrap();
        assert_eq!(tree3.root_node().kind(), "module");
    }

    #[test]
    fn test_parse_empty_source() {
        let mut parser = StructuralParser::new();
        let tree = parser.parse(b"", SupportedLanguage::Python).unwrap();
        let root = tree.root_node();
        assert_eq!(root.kind(), "module");
        assert_eq!(root.named_child_count(), 0);
    }

    #[test]
    fn test_parse_syntax_error() {
        let mut parser = StructuralParser::new();
        let source = b"def broken(:\n    nope\ndef valid(): pass\n";
        let tree = parser.parse(source, SupportedLanguage::Python);
        assert!(
            tree.is_ok(),
            "Parser should return tree even with syntax errors"
        );
        let tree = tree.unwrap();
        assert!(tree.root_node().has_error(), "Root should have error flag");
        // Tree-sitter still produces a partial AST
        assert!(
            tree.root_node().child_count() > 0,
            "Should still have child nodes"
        );
    }
}
