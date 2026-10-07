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
            ..Self::current()
        })
    }
}

impl fmt::Display for EmbeddingSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "model={} dim={} max_len={} special_tokens={} query_prompt={:?} doc_prompt={:?} normalisation={}",
            self.model_id,
            self.dim,
            self.max_len,
            self.special_tokens,
            self.query_prompt,
            self.doc_prompt,
            self.normalisation
        )
    }
}

/// Text embedded for a lesson. Every insert path and `nellie reembed` use this.
#[must_use]
pub fn lesson_embedding_text(title: &str, content: &str) -> String {
    format!("{title} {content}")
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
        assert_eq!(spec, EmbeddingSpec::current());
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

    #[test]
    fn embedding_text_formats() {
        assert_eq!(lesson_embedding_text("Title", "Body"), "Title Body");
        assert_eq!(checkpoint_embedding_text("working"), "working");
        assert_eq!(chunk_embedding_text("fn main() {}"), "fn main() {}");
    }
}
