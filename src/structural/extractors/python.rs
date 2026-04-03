//! Python symbol extractor.
//!
//! Extracts functions, classes, methods, imports, call sites, and test
//! functions from Python ASTs.

use crate::structural::extractor::{ExtractedSymbol, LanguageExtractor, SymbolKind};

/// Extracts symbols from Python source code.
pub struct PythonExtractor;

impl PythonExtractor {
    /// Helper: get text from a node.
    fn node_text<'a>(node: &tree_sitter::Node<'_>, source: &'a [u8]) -> &'a str {
        node.utf8_text(source).unwrap_or("")
    }

    /// Walk the AST and extract all symbols.
    fn walk_node(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope: Option<&str>,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        match node.kind() {
            "function_definition" => {
                Self::extract_function(node, source, scope, symbols);
            }
            "class_definition" => {
                Self::extract_class(node, source, symbols);
            }
            "import_statement" | "import_from_statement" => {
                Self::extract_import(node, source, symbols);
            }
            "call" => {
                Self::extract_call(node, source, scope, symbols);
            }
            _ => {}
        }

        // Recurse into children (unless already handled by extract_class or extract_function)
        if !matches!(node.kind(), "class_definition" | "function_definition") {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                Self::walk_node(&child, source, scope, symbols);
            }
        }
    }

    fn extract_function(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope: Option<&str>,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        let name = node
            .child_by_field_name("name")
            .map(|n| Self::node_text(&n, source).to_string())
            .unwrap_or_default();

        if name.is_empty() {
            return;
        }

        let kind = if scope.is_some() {
            SymbolKind::Method
        } else if name.starts_with("test_") {
            SymbolKind::TestFunction
        } else {
            SymbolKind::Function
        };

        // Build signature from parameters
        let signature = node
            .child_by_field_name("parameters")
            .map(|params| format!("def {}{}", name, Self::node_text(&params, source)));

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: scope.map(String::from),
            signature,
            language: "python".to_string(),
        });

        // Recurse into body for nested calls, but with this function as scope
        let body_scope = scope.map_or_else(|| name.clone(), |s| format!("{s}.{name}"));
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "block" {
                let mut block_cursor = child.walk();
                for block_child in child.children(&mut block_cursor) {
                    Self::walk_node(&block_child, source, Some(&body_scope), symbols);
                }
            }
        }
    }

    fn extract_class(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        let name = node
            .child_by_field_name("name")
            .map(|n| Self::node_text(&n, source).to_string())
            .unwrap_or_default();

        if name.is_empty() {
            return;
        }

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind: SymbolKind::Class,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: None,
            signature: None,
            language: "python".to_string(),
        });

        // Recurse into class body with class name as scope
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "block" {
                let mut block_cursor = child.walk();
                for block_child in child.children(&mut block_cursor) {
                    Self::walk_node(&block_child, source, Some(&name), symbols);
                }
            }
        }
    }

    fn extract_import(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        let text = Self::node_text(node, source).to_string();
        symbols.push(ExtractedSymbol {
            name: text,
            kind: SymbolKind::Import,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: None,
            signature: None,
            language: "python".to_string(),
        });
    }

    fn extract_call(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope: Option<&str>,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        let Some(func_node) = node.child_by_field_name("function") else {
            return;
        };

        let name = Self::node_text(&func_node, source).to_string();
        if name.is_empty() {
            return;
        }

        symbols.push(ExtractedSymbol {
            name,
            kind: SymbolKind::CallSite,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: scope.map(String::from),
            signature: None,
            language: "python".to_string(),
        });
    }
}

impl LanguageExtractor for PythonExtractor {
    fn extract(&self, tree: &tree_sitter::Tree, source: &[u8]) -> Vec<ExtractedSymbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            Self::walk_node(&child, source, None, &mut symbols);
        }
        symbols
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structural::language::SupportedLanguage;
    use crate::structural::parser::StructuralParser;

    fn extract_python(source: &str) -> Vec<ExtractedSymbol> {
        let mut parser = StructuralParser::new();
        let tree = parser
            .parse(source.as_bytes(), SupportedLanguage::Python)
            .unwrap();
        let extractor = PythonExtractor;
        extractor.extract(&tree, source.as_bytes())
    }

    #[test]
    fn test_extract_function() {
        let symbols = extract_python("def hello():\n    pass");
        let funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "hello");
        assert_eq!(funcs[0].start_line, 0);
        assert!(funcs[0].scope.is_none());
    }

    #[test]
    fn test_extract_test_function() {
        let symbols = extract_python("def test_thing():\n    pass");
        let tests: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::TestFunction)
            .collect();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "test_thing");
    }

    #[test]
    fn test_extract_class() {
        let symbols = extract_python("class Foo:\n    pass");
        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Foo");
    }

    #[test]
    fn test_extract_method() {
        let source = "class Foo:\n    def bar(self):\n        pass";
        let symbols = extract_python(source);
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "bar");
        assert_eq!(methods[0].scope.as_deref(), Some("Foo"));
    }

    #[test]
    fn test_extract_import() {
        let source = "import os\nfrom pathlib import Path";
        let symbols = extract_python(source);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn test_extract_call() {
        let source = "def foo():\n    bar()";
        let symbols = extract_python(source);
        let calls: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::CallSite)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bar");
        assert_eq!(calls[0].scope.as_deref(), Some("foo"));
    }

    #[test]
    fn test_extract_multi_class_file() {
        let source = "\
import os
from pathlib import Path

class Foo:
    def method_a(self):
        os.path.join('a', 'b')

    def test_method(self):
        pass

def standalone():
    Foo()

def test_standalone():
    pass
";
        let symbols = extract_python(source);

        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);

        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Foo");

        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
        assert!(methods.len() >= 2); // method_a, test_method

        let funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "standalone");

        let test_funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::TestFunction)
            .collect();
        assert_eq!(test_funcs.len(), 1);
        assert_eq!(test_funcs[0].name, "test_standalone");

        let calls: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::CallSite)
            .collect();
        assert!(!calls.is_empty());
    }
}
