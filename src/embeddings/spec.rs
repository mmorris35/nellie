//! Embedding specification: the settings a vector index was built with.
//!
//! Vectors produced under different settings (model, truncation length,
//! prompts, normalisation) are not comparable. Nellie records the spec that
//! built the active vector tables and refuses to mix vectors from different
//! specs. See `crate::storage::embedding_meta`.

use std::fmt;
use std::path::Path;

use super::model::{EMBEDDING_DIM, MAX_SEQ_LENGTH};

/// Hugging Face model id of the embedding model.
pub const MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// `special_tokens` value: `[CLS]` and `[SEP]` survive truncation.
pub const SPECIAL_TOKENS_INTACT: &str = "intact";

/// `special_tokens` value: long inputs lost their trailing `[SEP]`.
pub const SPECIAL_TOKENS_TRUNCATED: &str = "truncated";

/// `normalisation` value for L2-normalised vectors.
pub const NORMALISATION_L2: &str = "l2";

/// `lesson_vectors` value: one vector per lesson, embedded from
/// [`lesson_embedding_text`] cut at `max_len` tokens.
pub const LESSON_VECTORS_WHOLE: &str = "whole";

/// `lesson_vectors` value: a lesson longer than one window gets one vector
/// per section (see [`lesson_section_texts`]).
pub const LESSON_VECTORS_SECTIONS: &str = "sections-200-40";

/// The settings that determine what an embedding vector means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingSpec {
    /// Model identifier.
    pub model_id: String,
    /// Vector dimension.
    pub dim: usize,
    /// Maximum number of tokens embedded per text (including special tokens).
    pub max_len: usize,
    /// Whether special tokens survive truncation (`intact` / `truncated`).
    pub special_tokens: String,
    /// Prompt prepended to queries.
    pub query_prompt: String,
    /// Prompt prepended to documents.
    pub doc_prompt: String,
    /// Vector normalisation.
    pub normalisation: String,
    /// How lessons are turned into vectors ([`LESSON_VECTORS_WHOLE`] or
    /// [`LESSON_VECTORS_SECTIONS`]).
    pub lesson_vectors: String,
}

impl EmbeddingSpec {
    /// The spec this binary embeds with.
    #[must_use]
    pub fn current() -> Self {
        Self {
            model_id: MODEL_ID.to_string(),
            dim: EMBEDDING_DIM,
            max_len: MAX_SEQ_LENGTH,
            special_tokens: SPECIAL_TOKENS_INTACT.to_string(),
            query_prompt: String::new(),
            doc_prompt: String::new(),
            normalisation: NORMALISATION_L2.to_string(),
            lesson_vectors: LESSON_VECTORS_SECTIONS.to_string(),
        }
    }

    /// Reconstruct the spec an older Nellie (which inherited truncation from
    /// `tokenizer.json`) actually embedded with.
    ///
    /// * `truncation.max_length` <= 256: every text was cut to that many
    ///   tokens with `[CLS]`/`[SEP]` intact.
    /// * truncation missing or > 256: texts were cut to 256 ids *after*
    ///   tokenization, which dropped the trailing `[SEP]` on long inputs.
    ///
    /// # Errors
    ///
    /// Returns a description of the problem if the file cannot be read or
    /// parsed.
    pub fn legacy_from_tokenizer_json(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let json: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;

        let truncation_len = json
            .get("truncation")
            .and_then(|t| t.get("max_length"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok());

        let (max_len, special_tokens) = match truncation_len {
            Some(n) if n <= MAX_SEQ_LENGTH => (n, SPECIAL_TOKENS_INTACT),
            _ => (MAX_SEQ_LENGTH, SPECIAL_TOKENS_TRUNCATED),
        };

        Ok(Self {
            max_len,
            special_tokens: special_tokens.to_string(),
            lesson_vectors: LESSON_VECTORS_WHOLE.to_string(),
            ..Self::current()
        })
    }
}

impl fmt::Display for EmbeddingSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "model={} dim={} max_len={} special_tokens={} query_prompt={:?} doc_prompt={:?} normalisation={} lesson_vectors={}",
            self.model_id,
            self.dim,
            self.max_len,
            self.special_tokens,
            self.query_prompt,
            self.doc_prompt,
            self.normalisation,
            self.lesson_vectors
        )
    }
}

/// Text embedded for a lesson. Every insert path and `nellie reembed` use this.
#[must_use]
pub fn lesson_embedding_text(title: &str, content: &str) -> String {
    format!("{title} {content}")
}

/// Target size of one lesson section, in tokens (excluding the title prefix
/// and special tokens).
pub const LESSON_SECTION_TOKENS: usize = 200;

/// Most tokens of trailing paragraphs repeated at the start of the next
/// section, so a fact that straddles a boundary is seen whole at least once.
pub const LESSON_SECTION_OVERLAP: usize = 40;

/// Smallest section budget used when a very long title leaves little room.
const MIN_SECTION_TOKENS: usize = 32;

/// Texts to embed for a lesson: one per vector.
///
/// A lesson whose [`lesson_embedding_text`] fits in [`MAX_SEQ_LENGTH`] tokens
/// (special tokens included) is embedded whole, exactly as before. A longer
/// lesson is split on paragraph boundaries (blank lines) into sections of at
/// most [`LESSON_SECTION_TOKENS`] content tokens; a paragraph longer than that
/// is split on line breaks, and a line longer than that at whitespace.
/// Consecutive sections share up to [`LESSON_SECTION_OVERLAP`] tokens of
/// whole trailing paragraphs. Each section is prefixed with the title the
/// same way [`lesson_embedding_text`] does, so every section embeds within
/// the window.
///
/// `count_tokens` returns the number of tokens in a text without special
/// tokens; pass the embedding tokenizer's count.
#[must_use]
pub fn lesson_section_texts(
    title: &str,
    content: &str,
    count_tokens: impl Fn(&str) -> usize,
) -> Vec<String> {
    let whole = lesson_embedding_text(title, content);
    // [CLS] and [SEP] take two positions of the window.
    let window = MAX_SEQ_LENGTH - 2;
    if count_tokens(&whole) <= window {
        return vec![whole];
    }

    // Room left for content once the title and its separator are in.
    let budget = window
        .saturating_sub(count_tokens(title).saturating_add(1))
        .clamp(MIN_SECTION_TOKENS, LESSON_SECTION_TOKENS);

    let mut pieces: Vec<(String, usize)> = Vec::new();
    for paragraph in split_paragraphs(content) {
        let n = count_tokens(paragraph);
        if n <= budget {
            pieces.push((paragraph.to_string(), n));
            continue;
        }
        for line in paragraph.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let n = count_tokens(line);
            if n <= budget {
                pieces.push((line.to_string(), n));
            } else {
                for part in split_at_whitespace(line, budget, &count_tokens) {
                    let n = count_tokens(&part);
                    pieces.push((part, n));
                }
            }
        }
    }

    let mut sections: Vec<Vec<&(String, usize)>> = Vec::new();
    let mut current: Vec<&(String, usize)> = Vec::new();
    let mut current_tokens = 0;
    for piece in &pieces {
        if !current.is_empty() && current_tokens + piece.1 > budget {
            // Carry whole trailing pieces worth at most the overlap.
            let mut carry: Vec<&(String, usize)> = Vec::new();
            let mut carry_tokens = 0;
            for prev in current.iter().rev() {
                if carry_tokens + prev.1 > LESSON_SECTION_OVERLAP {
                    break;
                }
                carry.insert(0, *prev);
                carry_tokens += prev.1;
            }
            if carry_tokens + piece.1 > budget {
                carry.clear();
                carry_tokens = 0;
            }
            sections.push(std::mem::replace(&mut current, carry));
            current_tokens = carry_tokens;
        }
        current.push(piece);
        current_tokens += piece.1;
    }
    if !current.is_empty() {
        sections.push(current);
    }
    if sections.is_empty() {
        return vec![whole];
    }

    sections
        .iter()
        .map(|section| {
            let body: Vec<&str> = section.iter().map(|(text, _)| text.as_str()).collect();
            lesson_embedding_text(title, &body.join("\n\n"))
        })
        .collect()
}

/// Non-empty paragraphs (separated by blank lines), trimmed.
fn split_paragraphs(content: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        if line.trim().is_empty() {
            if let Some(s) = start.take() {
                out.push(content[s..end].trim());
            }
        } else {
            if start.is_none() {
                start = Some(offset);
            }
            end = offset + line.len();
        }
        offset += line.len();
    }
    if let Some(s) = start {
        out.push(content[s..end].trim());
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Split `text` into parts of at most `budget` tokens, cutting at whitespace
/// (or inside a word if a single word is longer than the budget).
///
/// Word counts are summed, which is exact for tokenizers that split on
/// whitespace first (as BERT's does) and keeps this linear in the text length.
fn split_at_whitespace(
    text: &str,
    budget: usize,
    count_tokens: &impl Fn(&str) -> usize,
) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut current_tokens = 0;
    for word in text.split_whitespace() {
        let n = count_tokens(word);
        if !current.is_empty() && current_tokens + n > budget {
            parts.push(current.join(" "));
            current.clear();
            current_tokens = 0;
        }
        if n > budget {
            // One enormous "word" (a URL, a hash dump): cut it by characters.
            parts.extend(cut_by_chars(word, budget, count_tokens));
            continue;
        }
        current.push(word);
        current_tokens += n;
    }
    if !current.is_empty() {
        parts.push(current.join(" "));
    }
    parts
}

/// Cut a single word into pieces of at most `budget` tokens (at least one
/// character each), finding each cut by binary search.
fn cut_by_chars(word: &str, budget: usize, count_tokens: &impl Fn(&str) -> usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = word;
    while !rest.is_empty() {
        // Candidate end offsets: every char boundary within a generous window.
        let ends: Vec<usize> = rest
            .char_indices()
            .skip(1)
            .map(|(i, _)| i)
            .chain(std::iter::once(rest.len()))
            .take(budget.saturating_mul(64).max(1))
            .collect();
        let (mut lo, mut hi) = (0, ends.len() - 1);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if count_tokens(&rest[..ends[mid]]) <= budget {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        parts.push(rest[..ends[lo]].to_string());
        rest = &rest[ends[lo]..];
    }
    parts
}

/// Text embedded for a checkpoint. Every insert path and `nellie reembed` use this.
#[must_use]
pub fn checkpoint_embedding_text(working_on: &str) -> String {
    working_on.to_string()
}

/// Text embedded for a code chunk. Every insert path and `nellie reembed` use this.
#[must_use]
pub fn chunk_embedding_text(content: &str) -> String {
    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tokenizer_json(dir: &Path, truncation: &str) -> std::path::PathBuf {
        let path = dir.join("tokenizer.json");
        std::fs::write(
            &path,
            format!(r#"{{"version":"1.0","truncation":{truncation},"padding":null}}"#),
        )
        .unwrap();
        path
    }

    #[test]
    fn current_spec_values() {
        let spec = EmbeddingSpec::current();
        assert_eq!(spec.model_id, MODEL_ID);
        assert_eq!(spec.dim, 384);
        assert_eq!(spec.max_len, 256);
        assert_eq!(spec.special_tokens, "intact");
        assert_eq!(spec.query_prompt, "");
        assert_eq!(spec.doc_prompt, "");
        assert_eq!(spec.normalisation, "l2");
    }

    #[test]
    fn legacy_spec_short_truncation_is_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_tokenizer_json(
            dir.path(),
            r#"{"max_length":128,"strategy":"LongestFirst","stride":0}"#,
        );
        let spec = EmbeddingSpec::legacy_from_tokenizer_json(&path).unwrap();
        assert_eq!(spec.max_len, 128);
        assert_eq!(spec.special_tokens, "intact");
        assert_ne!(spec, EmbeddingSpec::current());
    }

    #[test]
    fn legacy_spec_256_truncation_matches_current() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_tokenizer_json(
            dir.path(),
            r#"{"max_length":256,"strategy":"LongestFirst","stride":0}"#,
        );
        let spec = EmbeddingSpec::legacy_from_tokenizer_json(&path).unwrap();
        // Same vectors as today for chunks and checkpoints, but lessons were
        // embedded whole.
        assert_eq!(spec.lesson_vectors, LESSON_VECTORS_WHOLE);
        assert_ne!(spec, EmbeddingSpec::current());
        assert_eq!(
            EmbeddingSpec {
                lesson_vectors: LESSON_VECTORS_SECTIONS.to_string(),
                ..spec
            },
            EmbeddingSpec::current()
        );
    }

    #[test]
    fn legacy_spec_missing_truncation_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_tokenizer_json(dir.path(), "null");
        let spec = EmbeddingSpec::legacy_from_tokenizer_json(&path).unwrap();
        assert_eq!(spec.max_len, 256);
        assert_eq!(spec.special_tokens, "truncated");
    }

    #[test]
    fn legacy_spec_long_truncation_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_tokenizer_json(
            dir.path(),
            r#"{"max_length":512,"strategy":"LongestFirst","stride":0}"#,
        );
        let spec = EmbeddingSpec::legacy_from_tokenizer_json(&path).unwrap();
        assert_eq!(spec.max_len, 256);
        assert_eq!(spec.special_tokens, "truncated");
    }

    #[test]
    fn legacy_spec_missing_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(EmbeddingSpec::legacy_from_tokenizer_json(&dir.path().join("nope.json")).is_err());
    }

    /// Token count for tests: one token per whitespace-separated word.
    fn words(text: &str) -> usize {
        text.split_whitespace().count()
    }

    fn paragraph(tag: &str, n: usize) -> String {
        (0..n)
            .map(|i| format!("{tag}{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn short_lesson_is_one_whole_text() {
        let texts = lesson_section_texts("Title", "a short body", words);
        assert_eq!(texts, vec![lesson_embedding_text("Title", "a short body")]);
        // Exactly at the window (254 content tokens incl. the title) is still whole.
        let body = paragraph("w", MAX_SEQ_LENGTH - 2 - 1);
        assert_eq!(lesson_section_texts("Title", &body, words).len(), 1);
        let body = paragraph("w", MAX_SEQ_LENGTH - 2);
        assert!(lesson_section_texts("Title", &body, words).len() > 1);
    }

    #[test]
    fn long_lesson_splits_on_paragraphs_with_title_and_overlap() {
        let paras: Vec<String> = (0..8).map(|p| paragraph(&format!("p{p}x"), 70)).collect();
        let tail = paragraph("tail", 20);
        let content = format!(
            "{}\n\n{tail}\n\n\n{}",
            paras[..4].join("\n\n"),
            paras[4..].join("\n\n")
        );
        let texts = lesson_section_texts("My title", &content, words);
        assert!(texts.len() >= 3, "{} sections", texts.len());
        for t in &texts {
            assert!(t.starts_with("My title "), "{t}");
            assert!(words(t) <= MAX_SEQ_LENGTH - 2, "{} tokens", words(t));
            assert!(words(t) - 2 <= LESSON_SECTION_TOKENS);
        }
        // Every paragraph is in some section, never cut.
        for p in paras.iter().chain([&tail]) {
            assert!(texts.iter().any(|t| t.contains(p.as_str())), "missing {p}");
        }
        // The short paragraph before a boundary is repeated as overlap.
        assert_eq!(
            texts.iter().filter(|t| t.contains(tail.as_str())).count(),
            2
        );
    }

    #[test]
    fn oversized_paragraphs_split_on_lines_then_words() {
        // One paragraph of short lines, and one line of 1,000 words.
        let lines: Vec<String> = (0..30).map(|i| paragraph(&format!("l{i}x"), 15)).collect();
        let content = format!("{}\n\n{}", lines.join("\n"), paragraph("long", 1000));
        let texts = lesson_section_texts("T", &content, words);
        for t in &texts {
            assert!(words(t) <= LESSON_SECTION_TOKENS + 1, "{} tokens", words(t));
        }
        for l in &lines {
            assert!(texts.iter().any(|t| t.contains(l.as_str())), "missing {l}");
        }
        for i in [0, 499, 999] {
            let w = format!("long{i}");
            assert!(
                texts.iter().any(|t| t.split_whitespace().any(|x| x == w)),
                "missing {w}"
            );
        }
    }

    #[test]
    fn enormous_word_is_cut_by_characters() {
        // Count one token per 4 characters, so a 4,000-char word is 1,000 tokens.
        let chars = |t: &str| t.split_whitespace().map(|w| w.len().div_ceil(4)).sum();
        let content = "x".repeat(4000);
        let texts = lesson_section_texts("T", &content, chars);
        assert!(texts.len() >= 5);
        let total: usize = texts.iter().map(|t| t.len() - "T ".len()).sum();
        assert_eq!(total, 4000);
        for t in &texts {
            assert!(chars(t) <= MAX_SEQ_LENGTH - 2);
        }
    }

    #[test]
    fn long_title_still_fits_the_window() {
        let title = paragraph("title", 240);
        let texts = lesson_section_texts(&title, &paragraph("body", 300), words);
        for t in &texts {
            assert!(t.starts_with(&title));
        }
        // The section budget never drops below the floor, so progress is made.
        assert!(texts.len() <= 300 / MIN_SECTION_TOKENS + 1);
    }

    #[test]
    fn embedding_text_formats() {
        assert_eq!(lesson_embedding_text("Title", "Body"), "Title Body");
        assert_eq!(checkpoint_embedding_text("working"), "working");
        assert_eq!(chunk_embedding_text("fn main() {}"), "fn main() {}");
    }
}
