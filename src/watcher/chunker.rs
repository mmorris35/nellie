//! Code chunking for indexing.

use std::path::Path;

/// Chunk of code from a file.
#[derive(Debug, Clone)]
pub struct CodeChunk {
    /// Starting line (1-based).
    pub start_line: usize,
    /// Ending line (1-based, inclusive).
    pub end_line: usize,
    /// Chunk content.
    pub content: String,
    /// Chunk index within file.
    pub index: usize,
}

/// Chunking configuration.
#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Target chunk size in lines.
    pub target_lines: usize,
    /// Minimum chunk size in lines.
    pub min_lines: usize,
    /// Maximum chunk size in lines.
    pub max_lines: usize,
    /// Overlap between chunks in lines.
    pub overlap_lines: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            target_lines: 50,
            min_lines: 10,
            max_lines: 100,
            overlap_lines: 5,
        }
    }
}

/// Code chunker.
pub struct Chunker {
    config: ChunkerConfig,
}

impl Chunker {
    /// Create a new chunker with config.
    #[must_use]
    pub const fn new(config: ChunkerConfig) -> Self {
        Self { config }
    }

    /// Create a chunker with default config.
    #[must_use]
    pub fn default_chunker() -> Self {
        Self::new(ChunkerConfig::default())
    }

    /// Chunk file content into pieces.
    #[must_use]
    pub fn chunk_content(&self, content: &str, _language: Option<&str>) -> Vec<CodeChunk> {
        let lines: Vec<&str> = content.lines().collect();

        if lines.is_empty() {
            return Vec::new();
        }

        // For small files, return as single chunk
        if lines.len() <= self.config.max_lines {
            return vec![CodeChunk {
                start_line: 1,
                end_line: lines.len(),
                content: content.to_string(),
                index: 0,
            }];
        }

        let mut chunks = Vec::new();
        let mut start = 0;
        let mut index = 0;

        while start < lines.len() {
            let end = self.find_chunk_end(&lines, start);
            let chunk_lines = &lines[start..end];

            chunks.push(CodeChunk {
                start_line: start + 1,
                end_line: end,
                content: chunk_lines.join("\n"),
                index,
            });

            index += 1;

            // Move start with overlap
            let next_start = if end >= lines.len() {
                lines.len()
            } else {
                (end - self.config.overlap_lines).max(start + 1)
            };

            if next_start <= start {
                break;
            }
            start = next_start;
        }

        chunks
    }

    /// Find a good end point for a chunk.
    fn find_chunk_end(&self, lines: &[&str], start: usize) -> usize {
        let ideal_end = (start + self.config.target_lines).min(lines.len());
        let max_end = (start + self.config.max_lines).min(lines.len());

        // Try to find a good break point
        for i in (ideal_end..=max_end).rev() {
            if Self::is_good_break_point(lines, i) {
                return i;
            }
        }

        // Fall back to ideal end
        ideal_end
    }

    /// Check if a line is a good place to break.
    fn is_good_break_point(lines: &[&str], pos: usize) -> bool {
        if pos >= lines.len() {
            return true;
        }

        let line = lines[pos].trim();

        // Empty lines are good breaks
        if line.is_empty() {
            return true;
        }

        // Lines starting with certain patterns are good breaks
        let good_starts = [
            "fn ",
            "pub fn ",
            "async fn ",
            "pub async fn ",
            "impl ",
            "pub struct ",
            "struct ",
            "enum ",
            "pub enum ",
            "trait ",
            "pub trait ",
            "mod ",
            "pub mod ",
            "def ",
            "class ",
            "async def ",
            "function ",
            "const ",
            "let ",
            "export ",
            "public ",
            "private ",
            "protected ",
            "#",
            "//",
            "/*",
            "///",
        ];

        good_starts.iter().any(|s| line.starts_with(s))
    }

    /// Chunk a file from path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn chunk_file(&self, path: &Path) -> std::io::Result<Vec<CodeChunk>> {
        let content = std::fs::read_to_string(path)?;
        let language = super::filter::FileFilter::detect_language(path);
        Ok(self.chunk_content(&content, language))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_small_file() {
        let chunker = Chunker::default_chunker();
        let content = "line 1\nline 2\nline 3";

        let chunks = chunker.chunk_content(content, Some("rust"));

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
        assert_eq!(chunks[0].index, 0);
    }

    #[test]
    fn test_chunk_large_file() {
        let chunker = Chunker::new(ChunkerConfig {
            target_lines: 10,
            min_lines: 5,
            max_lines: 15,
            overlap_lines: 2,
        });

        // Create 30 lines
        let content: String = (1..=30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        let chunks = chunker.chunk_content(&content, Some("rust"));

        assert!(chunks.len() > 1);
        // Check all content is covered
        assert_eq!(chunks[0].start_line, 1);
        assert!(chunks.last().unwrap().end_line >= 28);
    }

    #[test]
    fn test_chunk_empty_file() {
        let chunker = Chunker::default_chunker();
        let chunks = chunker.chunk_content("", None);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_good_break_points() {
        let lines = vec![
            "fn main() {",
            "    println!(\"hello\");",
            "}",
            "",
            "fn other() {",
        ];

        // Empty line is good break
        assert!(Chunker::is_good_break_point(&lines, 3));
        // Function start is good break
        assert!(Chunker::is_good_break_point(&lines, 4));
        // Middle of function is not good break
        assert!(!Chunker::is_good_break_point(&lines, 1));
    }
}
