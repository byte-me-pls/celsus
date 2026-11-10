//! Translating a PDF end to end: find the blocks, ask the model in batches, check the answers, write the pages.
//!
//! Only block text and a little context go to the model. Translations are cached by source text, so translating
//! the same text again (a second run, a repeated header) costs nothing.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::extract::extract_page;
use crate::fonts::FontSet;
use crate::render::{BookWriter, Issue};
use crate::translate::{
    BookContext, TranslateError, TranslationRequest, Translator, Usage, check, describe,
    strip_source_glosses,
};
use crate::{BlockId, Language};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Options {
    pub from: Option<Language>,
    pub to: Language,
    pub book: BookContext,
    /// Pages to translate, 1-based. Empty means the whole document.
    pub pages: Vec<usize>,
    /// Characters of source text per request.
    pub batch_chars: usize,
    /// Seconds to wait before asking again after the provider failed; the wait grows with every try.
    #[serde(default = "default_retry_secs")]
    pub retry_secs: u64,
    /// Pictures redrawn in the target language (image XObject, JPEG), put in place of the originals.
    #[serde(skip)]
    pub images: Vec<(celsus_pdf::ObjRef, Vec<u8>)>,
}

fn default_retry_secs() -> u64 {
    5
}

impl Options {
    pub fn new(to: Language) -> Self {
        Options {
            from: None,
            to,
            book: BookContext::default(),
            pages: Vec::new(),
            batch_chars: 5000,
            retry_secs: default_retry_secs(),
            images: Vec::new(),
        }
    }
}

/// What is happening, for a progress display.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Progress {
    Started {
        pages: usize,
    },
    PageStarted {
        page: usize,
        blocks: usize,
    },
    PageDone {
        page: usize,
        translated: usize,
        replaced: usize,
        kept: usize,
        /// Blocks that should have been translated and got no usable answer. A page with any is not finished.
        missing: usize,
    },
    Waiting {
        seconds: u64,
    },
    Note {
        message: String,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Outcome {
    pub pages: usize,
    pub replaced: usize,
    pub kept: usize,
    pub requests: usize,
    pub usage: Usage,
    pub issues: Vec<(u32, Issue)>,
    pub cancelled: bool,
    /// Why the run stopped early, if it did. The file still holds every page that was finished before that.
    pub failed: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Pdf(#[from] celsus_pdf::Error),
    #[error(transparent)]
    Translate(#[from] TranslateError),
    #[error("no usable fonts were found")]
    NoFonts,
}

struct Session<'a> {
    provider: &'a dyn Translator,
    options: &'a Options,
    cache: &'a mut dyn Cache,
    usage: Usage,
    requests: usize,
    progress: &'a mut dyn FnMut(Progress),
    cancel: &'a AtomicBool,
    /// Set once the run is stopped or has failed: from then on nothing is asked, and only pages whose text is
    /// all in the cache are written.
    offline: bool,
}

impl Session<'_> {
    /// Wait, giving up early when the job is stopped.
    fn wait(&self, seconds: u64) {
        let until = std::time::Instant::now() + Duration::from_secs(seconds);
        while std::time::Instant::now() < until && !self.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn key(&self, source: &str) -> String {
        cache_key(&self.options.to, source)
    }

    fn ask(
        &mut self,
        blocks: Vec<(BlockId, String)>,
        before: &str,
        after: &str,
        feedback: Vec<String>,
    ) -> Result<Vec<String>, TranslateError> {
        let request = TranslationRequest {
            source_language: self.options.from.clone(),
            target_language: self.options.to.clone(),
            book: self.options.book.clone(),
            previous_text: before.to_owned(),
            next_text: after.to_owned(),
            blocks,
            feedback,
        };
        for attempt in 0..6u64 {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(TranslateError::Provider("cancelled".into()));
            }
            self.requests += 1;
            match self.provider.translate(&request) {
                Ok((texts, usage)) => {
                    self.usage.prompt_tokens += usage.prompt_tokens;
                    self.usage.completion_tokens += usage.completion_tokens;
                    return Ok(texts);
                }
                Err(TranslateError::RateLimited { retry_after_secs }) => {
                    (self.progress)(Progress::Waiting {
                        seconds: retry_after_secs,
                    });
                    self.wait(retry_after_secs);
                }
                Err(TranslateError::Provider(e)) if attempt < 2 => {
                    (self.progress)(Progress::Note {
                        message: format!("provider error, retrying: {e}"),
                    });
                    self.wait(self.options.retry_secs * (attempt + 1));
                }
                Err(e) => return Err(e),
            }
        }
        Err(TranslateError::Provider(
            "the provider kept refusing requests: the rate limit is too low or the quota is used up".into(),
        ))
    }

    /// Translate blocks in batches; a batch that does not line up, or a translation that fails the local checks,
    /// is asked again one block at a time with what was wrong. A provider that cannot be reached is an error:
    /// carrying on would leave text untranslated without anyone noticing.
    fn translate(
        &mut self,
        blocks: &[(BlockId, String)],
        before: &str,
        after: &str,
    ) -> Result<BTreeMap<BlockId, String>, TranslateError> {
        let mut out = BTreeMap::new();
        let mut pending: Vec<(BlockId, String)> = Vec::new();
        for (id, source) in blocks {
            match self.cache.get(&self.key(source)) {
                Some(t) => {
                    out.insert(id.clone(), t.clone());
                }
                None => pending.push((id.clone(), source.clone())),
            }
        }
        if self.offline {
            return Ok(out);
        }
        let mut batches: Vec<Vec<(BlockId, String)>> = vec![Vec::new()];
        let mut size = 0;
        for block in pending {
            if size + block.1.len() > self.options.batch_chars
                && !batches.last().unwrap().is_empty()
            {
                batches.push(Vec::new());
                size = 0;
            }
            size += block.1.len();
            batches.last_mut().unwrap().push(block);
        }
        for batch in batches.into_iter().filter(|b| !b.is_empty()) {
            let mut retry_alone = Vec::new();
            match self.ask(batch.clone(), before, after, Vec::new()) {
                Ok(texts) => {
                    for ((id, source), text) in batch.into_iter().zip(texts) {
                        let text = strip_source_glosses(&source, &text);
                        if check(&source, &text).is_empty() {
                            let key = self.key(&source);
                            self.cache.put(&key, &text);
                            out.insert(id, text);
                        } else {
                            retry_alone.push((id, source, Some(text)));
                        }
                    }
                }
                Err(e @ TranslateError::Fatal(_)) => return Err(e),
                Err(_) if self.cancel.load(Ordering::Relaxed) => return Ok(out),
                Err(_) => retry_alone.extend(batch.into_iter().map(|(id, s)| (id, s, None))),
            }
            for (id, source, first) in retry_alone {
                let feedback: Vec<String> = first
                    .as_deref()
                    .map(|f| check(&source, f).iter().map(describe).collect())
                    .unwrap_or_default();
                let second =
                    match self.ask(vec![(id.clone(), source.clone())], before, after, feedback) {
                        Err(_) if self.cancel.load(Ordering::Relaxed) => return Ok(out),
                        Err(e @ (TranslateError::Fatal(_) | TranslateError::Provider(_))) => {
                            return Err(e);
                        }
                        other => other,
                    }
                    .ok()
                    .and_then(|t| t.into_iter().next())
                    .map(|t| strip_source_glosses(&source, &t));
                let chosen = match (first, second) {
                    (_, Some(t)) if check(&source, &t).is_empty() => Some(t),
                    (Some(f), Some(t)) => {
                        Some(if check(&source, &f).len() <= check(&source, &t).len() {
                            f
                        } else {
                            t
                        })
                    }
                    (f, t) => t.or(f),
                };
                if let Some(t) = chosen {
                    let key = self.key(&source);
                    self.cache.put(&key, &t);
                    out.insert(id, t);
                }
            }
        }
        Ok(out)
    }
}

/// The cache key of a block: the same text in the same target language is translated once, whoever translates
/// it (a model through an API or an assistant in a chat). Whose cache it is, one person's or one book's, is up to
/// the [`Cache`].
pub fn cache_key(to: &Language, source: &str) -> String {
    format!("{to}|{source}")
}

/// Translations made before a block's lines were kept apart (tables of contents, lists) are stored under the
/// joined text, and those made before the words in another style were told apart under the text alone. Copy them
/// under the current key so earlier work, paid for or done by hand, is not lost.
pub fn adopt_earlier(cache: &mut dyn Cache, to: &Language, blocks: &[crate::layout::TextBlock]) {
    let changed = |b: &&crate::layout::TextBlock| !b.protected && (b.keeps_lines() || !b.inline.is_empty());
    for b in blocks.iter().filter(changed) {
        let key = cache_key(to, &b.source());
        if cache.get(&key).is_some() {
            continue;
        }
        let joined = crate::layout::join_lines(b.lines.iter().map(|l| l.text.as_str()));
        let earlier = [b.plain(), joined]
            .iter()
            .find_map(|text| cache.get(&cache_key(to, text)));
        if let Some(t) = earlier {
            cache.put(&key, &t);
        }
    }
}

/// Where translations are kept between runs, keyed by [`cache_key`].
pub trait Cache {
    fn get(&self, key: &str) -> Option<String>;
    fn put(&mut self, key: &str, translation: &str);
}

impl Cache for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        BTreeMap::get(self, key).cloned()
    }
    fn put(&mut self, key: &str, translation: &str) {
        self.insert(key.to_owned(), translation.to_owned());
    }
}

/// Translate `doc` and return the new file. `cache` maps source text to translations and is updated as pages
/// finish; `cancel` stops after the current request and still writes what was done.
///
/// A run that cannot go on (the provider is unreachable, the key is refused, a page cannot be read) also returns
/// the file, with [`Outcome::failed`] saying why. Either way the pages after the stop that are already in the
/// cache are still written, so a book translated earlier does not lose them.
pub fn translate_document(
    doc: &celsus_pdf::Document,
    fonts: &FontSet,
    provider: &dyn Translator,
    options: &Options,
    cache: &mut dyn Cache,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, Outcome), PipelineError> {
    if fonts.is_empty() {
        return Err(PipelineError::NoFonts);
    }
    let pages = doc.pages()?;
    let mut wanted: Vec<usize> = if options.pages.is_empty() {
        (1..=pages.len()).collect()
    } else {
        options
            .pages
            .iter()
            .copied()
            .filter(|&n| n >= 1 && n <= pages.len())
            .collect()
    };
    wanted.sort_unstable();
    wanted.dedup();
    progress(Progress::Started {
        pages: wanted.len(),
    });

    let page_text = |n: usize| -> String {
        if n == 0 || n > pages.len() {
            return String::new();
        }
        extract_page(doc, &pages[n - 1], n as u32)
            .map(|e| {
                e.layout
                    .blocks
                    .iter()
                    .map(|b| b.plain())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    };
    let mut writer = BookWriter::new(doc, fonts);
    for (id, jpeg) in &options.images {
        writer.replace_image(*id, jpeg)?;
    }
    let mut outcome = Outcome::default();
    let mut session = Session {
        provider,
        options,
        cache,
        usage: Usage::default(),
        requests: 0,
        progress,
        cancel,
        offline: false,
    };
    for n in wanted {
        if !session.offline && cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            session.offline = true;
        }
        let page = &pages[n - 1];
        let extracted = match extract_page(doc, page, n as u32) {
            Ok(e) => e,
            Err(e) => {
                if !session.offline {
                    outcome.failed = Some(format!("page {n}: {e}"));
                    session.offline = true;
                }
                continue;
            }
        };
        adopt_earlier(session.cache, &options.to, &extracted.layout.blocks);
        let blocks: Vec<(BlockId, String)> = extracted
            .layout
            .blocks
            .iter()
            .filter(|b| !b.protected)
            .map(|b| (b.id.clone(), b.source()))
            .collect();
        let (mut before, mut after) = (String::new(), String::new());
        if !session.offline {
            (session.progress)(Progress::PageStarted {
                page: n,
                blocks: blocks.len(),
            });
            let t = page_text(n - 1);
            let chars: Vec<char> = t.chars().collect();
            before = chars[chars.len().saturating_sub(1200)..].iter().collect();
            after = page_text(n + 1).chars().take(1200).collect();
        }
        let translations = match session.translate(&blocks, &before, &after) {
            Ok(t) => t,
            Err(e) => {
                outcome.failed = Some(format!("page {n}: {e}"));
                session.offline = true;
                // What the page already got is in the cache; the rest of it is missing.
                session.translate(&blocks, "", "").unwrap_or_default()
            }
        };
        let missing = blocks
            .iter()
            .filter(|(id, _)| !translations.contains_key(id))
            .count();
        if missing > 0 && !session.offline && cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            session.offline = true;
        }
        if missing > 0 && session.offline {
            // A page that is only partly translated stays as it was.
            continue;
        }
        let report = match writer.translate_page(page, &extracted, &translations, &options.to) {
            Ok(r) => r,
            Err(e) => {
                if !session.offline {
                    outcome.failed = Some(format!("page {n}: {e}"));
                    session.offline = true;
                }
                continue;
            }
        };
        outcome.pages += 1;
        outcome.replaced += report.replaced.len();
        outcome.kept += report.kept.len();
        outcome
            .issues
            .extend(report.kept.iter().map(|i| (n as u32, i.clone())));
        (session.progress)(Progress::PageDone {
            page: n,
            translated: translations.len(),
            replaced: report.replaced.len(),
            kept: report.kept.len(),
            missing,
        });
    }
    outcome.requests = session.requests;
    outcome.usage = session.usage.clone();
    Ok((writer.finish()?, outcome))
}
