//! Rust symbol extractor.
//!
//! Extracts functions, structs, enums, traits, impl blocks, methods,
//! use declarations, call sites, and test functions from Rust ASTs.

use crate::structural::extractor::{ExtractedSymbol, LanguageExtractor, SymbolKind};

/// Extracts symbols from Rust source code.
pub struct RustExtractor;

impl RustExtractor {
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
            "function_item" => {
                Self::extract_function(node, source, scope, symbols);
            }
            "struct_item" | "enum_item" | "trait_item" => {
                Self::extract_type_def(node, source, symbols);
            }
            "impl_item" => {
                Self::extract_impl(node, source, symbols);
            }
            "use_declaration" => {
                Self::extract_use(node, source, symbols);
            }
            "call_expression" | "method_call_expression" => {
                Self::extract_call(node, source, scope, symbols);
            }
            _ => {}
        }

        // Recurse unless already handled
        if !matches!(node.kind(), "impl_item") {
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

        // Check for #[test] or #[tokio::test] attribute
        let is_test = Self::has_test_attribute(node, source);

        let kind = if is_test {
            SymbolKind::TestFunction
        } else if scope.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };

        let signature = node
            .child_by_field_name("parameters")
            .map(|params| format!("fn {}{}", name, Self::node_text(&params, source)));

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: scope.map(String::from),
            signature,
            language: "rust".to_string(),
        });

        // Recurse into body for calls
        let body_scope = if let Some(s) = scope {
            format!("{s}::{name}")
        } else {
            name
        };
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                Self::walk_node(&child, source, Some(&body_scope), symbols);
            }
        }
    }

    fn has_test_attribute(node: &tree_sitter::Node<'_>, source: &[u8]) -> bool {
        // Look for attribute_item siblings before this function
        if let Some(parent) = node.parent() {
            let mut cursor = parent.walk();
            let mut found_attr = false;
            for child in parent.children(&mut cursor) {
                if child.kind() == "attribute_item" {
                    let text = Self::node_text(&child, source);
                    if text.contains("test") {
                        found_attr = true;
                    }
                }
                if child.id() == node.id() && found_attr {
                    return true;
                }
                if child.kind() != "attribute_item" && child.kind() != "line_comment" {
                    found_attr = false;
                }
            }
        }
        false
    }

    fn extract_type_def(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        let name = node
            .child_by_field_name("name")
            .map(|n| Self::node_text(&n, source).to_string())
            .unwrap_or_default();

        if !name.is_empty() {
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Class,
                start_line: node.start_position().row as u32,
                end_line: node.end_position().row as u32,
                scope: None,
                signature: None,
                language: "rust".to_string(),
            });
        }
    }

    fn extract_impl(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        // Get the impl target type name
        let type_name = node
            .child_by_field_name("type")
            .map(|n| Self::node_text(&n, source).to_string())
            .unwrap_or_default();

        if type_name.is_empty() {
            return;
        }

        // Recurse into impl body with type name as scope
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                Self::walk_node(&child, source, Some(&type_name), symbols);
            }
        }
    }

    fn extract_use(
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
            language: "rust".to_string(),
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
            language: "rust".to_string(),
        });
    }
}

impl LanguageExtractor for RustExtractor {
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

    fn extract_rust(source: &str) -> Vec<ExtractedSymbol> {
        let mut parser = StructuralParser::new();
        let tree = parser
            .parse(source.as_bytes(), SupportedLanguage::Rust)
            .unwrap();
        let extractor = RustExtractor;
        extractor.extract(&tree, source.as_bytes())
    }

    #[test]
    fn test_extract_function() {
        let symbols = extract_rust("fn hello() {}");
        let funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "hello");
    }

    #[test]
    fn test_extract_struct() {
        let symbols = extract_rust("struct Foo { x: i32 }");
        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Foo");
    }

    #[test]
    fn test_extract_impl_methods() {
        let source = "struct Foo {}\nimpl Foo {\n    fn bar(&self) {}\n}";
        let symbols = extract_rust(source);
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "bar");
        assert_eq!(methods[0].scope.as_deref(), Some("Foo"));
    }

    #[test]
    fn test_extract_use_declaration() {
        let symbols = extract_rust("use std::path::PathBuf;");
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 1);
    }

    #[test]
    fn test_extract_test_function() {
        let source = "#[test]\nfn test_thing() {}";
        let symbols = extract_rust(source);
        let tests: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::TestFunction)
            .collect();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "test_thing");
    }

    #[test]
    fn test_extract_enum() {
        let symbols = extract_rust("enum Color { Red, Green, Blue }");
        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Color");
    }

    #[test]
    fn test_extract_trait() {
        let symbols = extract_rust("trait Display { fn fmt(&self); }");
        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Display");
    }
}
