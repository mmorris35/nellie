//! Structural code analysis using Tree-sitter.
//!
//! This module provides AST-based code analysis for extracting symbols,
//! relationships, and structural information from source files.
//!
//! # Supported Languages
//!
//! - Python (`.py`)
//! - TypeScript (`.ts`, `.tsx`)
//! - JavaScript (`.js`, `.jsx`)
//! - Rust (`.rs`)
//! - Go (`.go`)

pub mod extractor;
pub mod extractors;
pub mod graph_builder;
pub mod language;
pub mod parser;
pub mod storage;

pub use extractor::{
    extract_symbols, extract_symbols_from_file, ExtractedSymbol, ExtractionResult,
    LanguageExtractor, SymbolKind,
};
pub use language::SupportedLanguage;
pub use parser::StructuralParser;
