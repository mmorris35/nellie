use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_parse_python(c: &mut Criterion) {
    let source = r#"
import os
from pathlib import Path

class DataProcessor:
    def __init__(self, config):
        self.config = config
        self.data = []

    def process(self, input_data):
        result = self.validate(input_data)
        return self.transform(result)

    def validate(self, data):
        if not data:
            raise ValueError("empty data")
        return data

    def transform(self, data):
        return [item.strip() for item in data]

def main():
    processor = DataProcessor({})
    processor.process(["hello", "world"])

def test_process():
    p = DataProcessor({})
    assert p.process(["a"]) == ["a"]
"#;

    c.bench_function("parse_python_file", |b| {
        b.iter(|| {
            let mut parser = nellie::structural::StructuralParser::new();
            let tree = parser
                .parse(
                    black_box(source.as_bytes()),
                    nellie::structural::SupportedLanguage::Python,
                )
                .unwrap();
            let extractor = nellie::structural::extractors::python::PythonExtractor;
            let _symbols = nellie::structural::LanguageExtractor::extract(
                &extractor,
                &tree,
                source.as_bytes(),
            );
        });
    });
}

fn bench_parse_rust(c: &mut Criterion) {
    let source = r#"
use std::path::PathBuf;
use std::collections::HashMap;

pub struct Config {
    pub data_dir: PathBuf,
    pub port: u16,
}

impl Config {
    pub fn new() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            port: 8080,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("port cannot be 0".to_string());
        }
        Ok(())
    }
}

fn process(config: &Config) -> HashMap<String, String> {
    let mut map = HashMap::new();
    map.insert("port".to_string(), config.port.to_string());
    map
}

#[test]
fn test_validate() {
    let config = Config::new();
    assert!(config.validate().is_ok());
}
"#;

    c.bench_function("parse_rust_file", |b| {
        b.iter(|| {
            let mut parser = nellie::structural::StructuralParser::new();
            let tree = parser
                .parse(
                    black_box(source.as_bytes()),
                    nellie::structural::SupportedLanguage::Rust,
                )
                .unwrap();
            let extractor = nellie::structural::extractors::rust_lang::RustExtractor;
            let _symbols = nellie::structural::LanguageExtractor::extract(
                &extractor,
                &tree,
                source.as_bytes(),
            );
        });
    });
}

criterion_group!(benches, bench_parse_python, bench_parse_rust);
criterion_main!(benches);
