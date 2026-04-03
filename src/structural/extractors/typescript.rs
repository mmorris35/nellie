//! TypeScript and JavaScript symbol extractor.
//!
//! Handles both TypeScript (`.ts`, `.tsx`) and JavaScript (`.js`, `.jsx`).
//! The same extractor works for both because the TypeScript grammar is a
//! superset of JavaScript for the node kinds we care about.

use crate::structural::extractor::{ExtractedSymbol, LanguageExtractor, SymbolKind};

/// Extracts symbols from TypeScript and JavaScript source code.
pub struct TypeScriptExtractor {
    language_name: String,
}

impl TypeScriptExtractor {
    /// Create a new extractor with the given language name.
    #[must_use]
    pub fn new(language_name: &str) -> Self {
        Self {
            language_name: language_name.to_string(),
        }
    }

    fn node_text<'a>(node: &tree_sitter::Node<'_>, source: &'a [u8]) -> &'a str {
        node.utf8_text(source).unwrap_or("")
    }

    fn walk_node(
        &self,
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope: Option<&str>,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        match node.kind() {
            "function_declaration" => {
                self.extract_function_decl(node, source, scope, symbols);
            }
            "method_definition" => {
                self.extract_method(node, source, scope, symbols);
            }
            "class_declaration" => {
                self.extract_class(node, source, symbols);
            }
            "interface_declaration" => {
                self.extract_interface(node, source, symbols);
            }
            "import_statement" => {
                self.extract_import(node, source, symbols);
            }
            "call_expression" => {
                self.extract_call(node, source, scope, symbols);
            }
            "lexical_declaration" | "variable_declaration" => {
                self.extract_variable_decl(node, source, scope, symbols);
            }
            "expression_statement" => {
                // Check for top-level `it()`, `describe()`, `test()` calls
                self.extract_test_calls(node, source, scope, symbols);
            }
            _ => {}
        }

        // Recurse into children unless already handled
        if !matches!(node.kind(), "class_declaration" | "class_body") {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                self.walk_node(&child, source, scope, symbols);
            }
        }
    }

    fn extract_function_decl(
        &self,
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

        let kind = if name.starts_with("test") {
            SymbolKind::TestFunction
        } else {
            SymbolKind::Function
        };

        let signature = node
            .child_by_field_name("parameters")
            .map(|params| format!("function {}{}", name, Self::node_text(&params, source)));

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: scope.map(String::from),
            signature,
            language: self.language_name.clone(),
        });

        // Recurse into body
        self.recurse_body(node, source, &name, symbols);
    }

    fn extract_method(
        &self,
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

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind: SymbolKind::Method,
            start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            scope: scope.map(String::from),
            signature: None,
            language: self.language_name.clone(),
        });

        self.recurse_body(node, source, &name, symbols);
    }

    fn extract_class(
        &self,
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
            language: self.language_name.clone(),
        });

        // Recurse into class body
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "class_body" {
                let mut body_cursor = child.walk();
                for body_child in child.children(&mut body_cursor) {
                    self.walk_node(&body_child, source, Some(&name), symbols);
                }
            }
        }
    }

    fn extract_interface(
        &self,
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
                language: self.language_name.clone(),
            });
        }
    }

    fn extract_import(
        &self,
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
            language: self.language_name.clone(),
        });
    }

    fn extract_call(
        &self,
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
            language: self.language_name.clone(),
        });
    }

    fn extract_variable_decl(
        &self,
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope: Option<&str>,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        // Look for `const foo = () => {}` or `const foo = function() {}`
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator" {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| Self::node_text(&n, source).to_string())
                    .unwrap_or_default();
                let Some(val) = child.child_by_field_name("value") else {
                    continue;
                };
                if (val.kind() == "arrow_function" || val.kind() == "function") && !name.is_empty()
                {
                    let kind = if name.starts_with("test") {
                        SymbolKind::TestFunction
                    } else {
                        SymbolKind::Function
                    };
                    symbols.push(ExtractedSymbol {
                        name: name.clone(),
                        kind,
                        start_line: node.start_position().row as u32,
                        end_line: node.end_position().row as u32,
                        scope: scope.map(String::from),
                        signature: None,
                        language: self.language_name.clone(),
                    });
                    self.recurse_body(&val, source, &name, symbols);
                }
            }
        }
    }

    fn extract_test_calls(
        &self,
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope: Option<&str>,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        // Check for `it('...', ...)`, `test('...', ...)`, `describe('...', ...)`
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "call_expression" {
                let Some(func_node) = child.child_by_field_name("function") else {
                    continue;
                };
                let func_name = Self::node_text(&func_node, source);
                if matches!(func_name, "it" | "test" | "describe") {
                    // Get the first argument (test name string)
                    if let Some(args) = child.child_by_field_name("arguments") {
                        let test_name = args.named_child(0).map_or_else(
                            || func_name.to_string(),
                            |n| Self::node_text(&n, source).to_string(),
                        );

                        symbols.push(ExtractedSymbol {
                            name: format!("{func_name}({test_name})"),
                            kind: SymbolKind::TestFunction,
                            start_line: child.start_position().row as u32,
                            end_line: child.end_position().row as u32,
                            scope: scope.map(String::from),
                            signature: None,
                            language: self.language_name.clone(),
                        });
                    }
                }
            }
        }
    }

    fn recurse_body(
        &self,
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        scope_name: &str,
        symbols: &mut Vec<ExtractedSymbol>,
    ) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "statement_block" {
                let mut block_cursor = child.walk();
                for block_child in child.children(&mut block_cursor) {
                    self.walk_node(&block_child, source, Some(scope_name), symbols);
                }
            }
        }
    }
}

impl LanguageExtractor for TypeScriptExtractor {
    fn extract(&self, tree: &tree_sitter::Tree, source: &[u8]) -> Vec<ExtractedSymbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            self.walk_node(&child, source, None, &mut symbols);
        }
        symbols
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structural::language::SupportedLanguage;
    use crate::structural::parser::StructuralParser;

    fn extract_ts(source: &str) -> Vec<ExtractedSymbol> {
        let mut parser = StructuralParser::new();
        let tree = parser
            .parse(source.as_bytes(), SupportedLanguage::TypeScript)
            .unwrap();
        let extractor = TypeScriptExtractor::new("typescript");
        extractor.extract(&tree, source.as_bytes())
    }

    fn extract_js(source: &str) -> Vec<ExtractedSymbol> {
        let mut parser = StructuralParser::new();
        let tree = parser
            .parse(source.as_bytes(), SupportedLanguage::JavaScript)
            .unwrap();
        let extractor = TypeScriptExtractor::new("javascript");
        extractor.extract(&tree, source.as_bytes())
    }

    #[test]
    fn test_extract_function_declaration() {
        let symbols = extract_ts("function hello(): void {}");
        let funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "hello");
    }

    #[test]
    fn test_extract_arrow_function() {
        let symbols = extract_ts("const greet = () => {}");
        let funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "greet");
    }

    #[test]
    fn test_extract_class() {
        let symbols = extract_ts("class MyClass {\n  method() {}\n}");
        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "MyClass");
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].scope.as_deref(), Some("MyClass"));
    }

    #[test]
    fn test_extract_import() {
        let symbols = extract_ts("import { Foo } from 'bar'");
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 1);
    }

    #[test]
    fn test_extract_jest_test() {
        let symbols = extract_js("it('should work', () => {})");
        let tests: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::TestFunction)
            .collect();
        assert_eq!(tests.len(), 1);
        assert!(tests[0].name.starts_with("it("));
    }

    #[test]
    fn test_extract_js_function() {
        let symbols = extract_js("function hello() {}");
        let funcs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].language, "javascript");
    }
}
