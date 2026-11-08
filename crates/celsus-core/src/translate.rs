//! Translation contract between the pipeline and a model provider (Claude, OpenAI, Ollama, ...).

use std::collections::BTreeMap;
use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::{BlockId, Language};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    Pending,
    Translated,
    Protected,
    NeedsReview,
}

/// One block of source text and its translation state. Persisted as a checkpoint so work resumes after restarts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub id: BlockId,
    pub source: String,
    pub translation: Option<String>,
    pub status: SegmentStatus,
    pub attempts: u8,
}

/// Everything a provider needs to translate a batch of neighbouring blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationRequest {
    pub source_language: Option<Language>,
    pub target_language: Language,
    pub book_title: String,
    /// Term -> required translation, applied consistently across the book.
    pub glossary: BTreeMap<String, String>,
    pub previous_text: String,
    pub next_text: String,
    pub blocks: Vec<(BlockId, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("provider asked us to wait {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("provider returned {got} blocks for {expected}")]
    BoundaryMismatch { expected: usize, got: usize },
    #[error("provider error: {0}")]
    Provider(String),
}

/// A model provider. Must return exactly one translation per input block, in order.
pub trait Translator: Send + Sync {
    fn name(&self) -> &str;

    fn translate(
        &self,
        request: &TranslationRequest,
    ) -> impl Future<Output = Result<Vec<String>, TranslateError>> + Send;
}
