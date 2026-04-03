//! Go symbol extractor.
//!
//! Extracts functions, methods, type declarations, imports, call sites,
//! and test functions from Go ASTs.

use crate::structural::extractor::{ExtractedSymbol, LanguageExtractor, SymbolKind};

/// Extracts symbols from Go source code.
pub struct GoExtractor;

impl GoExtractor {
    fn node_text<'a>(node: &tree_sitter::Node<'_>, source: &'a [u8]) -> &'a str {
        node.utf8_text(source).unwrap_or("")
    }

    fn walk_node(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope: Option<&str>,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        match node.kind() {
            "function_declaration" => {
                Self::extract_function(node, source, symbols);
            }
            "method_declaration" => {
                Self::extract_method(node, source, symbols);
            }
            "type_declaration" => {
                Self::extract_type(node, source, symbols);
            }
            "import_declaration" => {
                Self::extract_import(node, source, symbols);
            }
            "call_expression" => {
                Self::extract_call(node, source, scope, symbols);
            }
            _ => {}
        }

        // Recurse unless already handled
        if !matches!(node.kind(), "function_declaration" | "method_declaration") {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                Self::walk_node(&child, source, scope, symbols);
            }
        }
    }

    fn extract_function(
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

        let kind = if name.starts_with("Test") || name.starts_with("Benchmark") {
            SymbolKind::TestFunction
        } else {
            SymbolKind::Function
        };

        let signature = node
            .child_by_field_name("parameters")
            .map(|params| format!("func {}{}", name, Self::node_text(&params, source)));

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: None,
            signature,
            language: "go".to_string(),
        });

        // Recurse into body for calls
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                Self::walk_node(&child, source, Some(&name), symbols);
            }
        }
    }

    fn extract_method(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        let name = node
            .child_by_field_name("name")
            .map(|n| Self::node_text(&n, source).to_string())
            .unwrap_or_default();

        // Extract receiver type for scope
        let receiver_type = node.child_by_field_name("receiver").and_then(|recv| {
            // The receiver is a parameter_list; find the type inside
            let mut cursor = recv.walk();
            for child in recv.named_children(&mut cursor) {
                // parameter_declaration -> type field
                if let Some(type_node) = child.child_by_field_name("type") {
                    let type_text = Self::node_text(&type_node, source);
                    // Strip pointer prefix
                    return Some(type_text.trim_start_matches('*').to_string());
                }
            }
            None
        });

        if name.is_empty() {
            return;
        }

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind: SymbolKind::Method,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: receiver_type.clone(),
            signature: None,
            language: "go".to_string(),
        });

        // Recurse into body
        let scope_name = if let Some(ref recv) = receiver_type {
            format!("{recv}.{name}")
        } else {
            name
        };
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                Self::walk_node(&child, source, Some(&scope_name), symbols);
            }
        }
    }

    fn extract_type(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        // type_declaration contains type_spec children
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "type_spec" {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| Self::node_text(&n, source).to_string())
                    .unwrap_or_default();

                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Class,
                        start_line: child.start_position().row as u32,
                        end_line: child.end_position().row as u32,
                        scope: None,
                        signature: None,
                        language: "go".to_string(),
                    });
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
            language: "go".to_string(),
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
            language: "go".to_string(),
        });
    }
}

impl LanguageExtractor for GoExtractor {
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

    fn extract_go(source: &str) -> Vec<ExtractedSymbol> {
        let mut parser = StructuralParser::new();
        let tree = parser
            .parse(source.as_bytes(), SupportedLanguage::Go)
            .unwrap();
        let extractor = GoExtractor;
        extractor.extract(&tree, source.as_bytes())
    }

    #[test]
    fn test_extract_function() {
        let source = "package main\nfunc hello() {}";
        let symbols = extract_go(source);
        let funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "hello");
    }

    #[test]
    fn test_extract_test_function() {
        let source = "package main\nfunc TestSomething(t *testing.T) {}";
        let symbols = extract_go(source);
        let tests: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::TestFunction)
            .collect();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "TestSomething");
    }

    #[test]
    fn test_extract_method() {
        let source = "package main\ntype Foo struct{}\nfunc (f *Foo) Bar() {}";
        let symbols = extract_go(source);
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "Bar");
        assert_eq!(methods[0].scope.as_deref(), Some("Foo"));
    }

    #[test]
    fn test_extract_type() {
        let source = "package main\ntype Foo struct{}";
        let symbols = extract_go(source);
        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Foo");
    }

    #[test]
    fn test_extract_import() {
        let source = "package main\nimport \"fmt\"";
        let symbols = extract_go(source);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 1);
    }
}
