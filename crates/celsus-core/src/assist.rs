//! Translation by an assistant in a chat (through MCP), whatever stores the library: Celsus hands out the text
//! block by block, the assistant sends the translations back, and Celsus checks them and writes the pages.
//!
//! The desktop library (SQLite) and the server (PostgreSQL) keep books and translations differently; both give
//! these functions the opened document, the pages already done and a translation cache.

use std::collections::{BTreeMap, BTreeSet};

use celsus_pdf::{Document, ObjRef};
use serde::{Deserialize, Serialize};

use crate::extract::{ExtractedPage, extract_page};
use crate::fonts::FontSet;
use crate::pipeline::{Cache, adopt_earlier, cache_key};
use crate::render::BookWriter;
use crate::translate::{check, describe, language_name, strip_source_glosses};
use crate::{BlockId, Language};

/// What the assistant is told with every batch.
pub fn instructions(to: &Language) -> String {
    let to = language_name(to);
    let terms = crate::translate::terms_rule(&to);
    format!(
        "Translate every block into {to}. Keep the author's meaning, negation, register and tone; write natural, \
         grammatical {to}. Translate short labels from figures and tables too. Keep numbers, formulas, code, \
         identifiers, URLs and proper names exactly as written. {terms} Words printed in another style come with \
         tags around them, such as <i>...</i>, <b>...</b>, <code>...</code> or <c>...</c>: keep every tag exactly as written, \
         around the words that say the same in the translation, closed and never one inside another, and add no \
         tags of your own. Do not add notes, explanations, glosses in \
         parentheses or ellipses that are not in the source. Use the book's subject to choose the right sense of a \
         word. Send every block back with \
         submit_translations, keeping its page and id. Blocks are independent pieces of the page: translate each \
         one on its own, even when a sentence continues in the next block."
    )
}

/// Read the language an assistant asked for. A name instead of a code ("turkish", "Deutsch") is refused with
/// the code to use, when one matches; a code Celsus does not know, with the nearest one it does.
pub fn parse_language(given: &str) -> Result<Language, String> {
    let given = given.trim();
    let refused = match given.parse::<Language>() {
        Ok(lang) => return Ok(lang),
        Err(e) => e,
    };
    let wanted = given.to_lowercase();
    let matches: Vec<&crate::languages::LanguageInfo> = crate::languages::LANGUAGES
        .iter()
        .filter(|l| {
            let english = l.english.to_lowercase();
            english == wanted
                || l.native.to_lowercase() == wanted
                // "Chinese" for "Chinese (Simplified)" and "Chinese (Traditional)".
                || english.strip_prefix(&wanted).is_some_and(|r| r.starts_with(" ("))
        })
        .collect();
    let mut message = format!(
        "\"{given}\" is not a language code. Give target_language as a code, such as tr, de, ar, ja, zh-hans or \
         pt-br."
    );
    match matches.as_slice() {
        [] if refused.is_unknown() => return Err(refused.to_string()),
        [] => {}
        [one] => message += &format!(" For {} it is {}.", one.english, one.code),
        many => {
            let codes: Vec<String> = many
                .iter()
                .map(|l| format!("{} for {}", l.code, l.english))
                .collect();
            message += &format!(" Use {}.", codes.join(", "));
        }
    }
    Err(message)
}

/// Whether a book written in `source` (a code, empty or "auto" when not known) is already in `to`: then there is
/// nothing to translate, and every page would only be handed out to be copied.
pub fn same_language(source: &str, to: &Language) -> bool {
    let source = source.trim().to_lowercase();
    if source.is_empty() || source == "auto" {
        return false;
    }
    let base = |l: &str| l.split(['-', '_']).next().unwrap_or("").to_owned();
    base(&source) == base(&to.as_str().to_lowercase())
}

/// The refusal for [`same_language`].
pub fn same_language_error(source: &str, to: &Language) -> String {
    format!(
        "This book is written in {} already (its source language is {source}), so there is nothing to translate. \
         Choose another target_language, or correct the book's source language if it is wrong.",
        language_name(to)
    )
}

/// Refuse page numbers that are not in a book of `book_pages` pages, rather than answering as if they were done.
pub fn check_pages(book_pages: usize, pages: &[usize]) -> Result<(), String> {
    match pages.iter().find(|&&p| p < 1 || p > book_pages) {
        Some(p) => Err(format!(
            "There is no page {p}: the book has {book_pages} pages."
        )),
        None => Ok(()),
    }
}

/// The book details an assistant sees with each batch.
#[derive(Debug, Clone, Default)]
pub struct BookInfo {
    pub title: String,
    pub subject: String,
    pub source_language: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingBlock {
    pub page: usize,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pending {
    pub title: String,
    pub subject: String,
    pub source_language: String,
    pub target_language: String,
    pub instructions: String,
    pub context_before: String,
    pub context_after: String,
    pub blocks: Vec<PendingBlock>,
    /// Pages in the requested range that are not finished yet (including the ones in this batch).
    pub pages_left: usize,
    pub pages_done: usize,
    /// Pages looked at whose every block already has a translation that counts: nothing is asked for them,
    /// and they only have to be written. Whoever keeps the book writes them and counts them as done.
    #[serde(skip)]
    pub ready: Vec<usize>,
    /// The pages among `ready` that have no text to translate at all.
    #[serde(skip)]
    pub empty: Vec<usize>,
    /// How many pages of this answer were finished from earlier translations instead of being asked for.
    #[serde(skip_serializing_if = "is_zero")]
    pub finished_from_earlier_translations: usize,
    /// Said to the assistant when pages were finished that way, so that it can tell whoever asked for them.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Pending {
    /// Count the ready pages as done once they are written, and say how many came from earlier translations.
    /// Pages being translated again that are ready have nothing on them to translate: no note for those.
    pub fn count_ready(&mut self, again: &BTreeSet<usize>) {
        self.pages_left -= self.ready.len();
        self.pages_done += self.ready.len();
        let earlier = self
            .ready
            .iter()
            .filter(|n| !self.empty.contains(n) && !again.contains(n))
            .count();
        if earlier == 0 {
            return;
        }
        self.finished_from_earlier_translations += earlier;
        self.note = format!(
            "{} page(s) were finished from translations Celsus already had of the same text and were not handed \
             out. If they should be translated anew, call translate_again for them first.",
            self.finished_from_earlier_translations
        );
    }
}

/// Which translations count as done. Any translation of the same text counts, from whichever book it was
/// made for, except on pages being translated again: there only the translations given since count.
///
/// What is given for a page being translated again is kept apart, under [`again_key`], until every block of the
/// page has its new translation: until then the translated PDF is written from the earlier translations, so the
/// page never shows new and old text side by side. `keys` holds the keys of what was given since, in either form:
/// the key of a translation that is in use already (given anew by a translation job, or a page finished), or the
/// key it is kept apart under.
#[derive(Debug, Clone, Copy)]
pub struct Fresh<'a> {
    pub pages: &'a BTreeSet<usize>,
    pub keys: &'a std::collections::HashSet<String>,
}

impl Fresh<'_> {
    /// Nothing is being translated again.
    pub fn none() -> Fresh<'static> {
        static PAGES: BTreeSet<usize> = BTreeSet::new();
        static KEYS: std::sync::LazyLock<std::collections::HashSet<String>> =
            std::sync::LazyLock::new(std::collections::HashSet::new);
        Fresh {
            pages: &PAGES,
            keys: &KEYS,
        }
    }

    fn known(&self, cache: &mut dyn Cache, page: usize, key: &str) -> bool {
        if self.pages.contains(&page) {
            let apart = again_key(key);
            self.keys.contains(key) || (self.keys.contains(&apart) && cache.get(&apart).is_some())
        } else {
            cache.get(key).is_some()
        }
    }
}

/// Where a new translation given for a page being translated again waits until the page is finished. A cache
/// key starts with a language code, which never holds `~`, so the two cannot meet.
pub fn again_key(key: &str) -> String {
    format!("~again|{key}")
}

/// Put the new translations of finished pages that were being translated again in use: copied from where they
/// waited (see [`Fresh`]) to their own keys. Returns those keys.
fn put_in_use(
    doc: &Document,
    all: &[celsus_pdf::Page],
    cache: &mut dyn Cache,
    to: &Language,
    pages: &[usize],
    keys: &std::collections::HashSet<String>,
) -> Result<Vec<String>, String> {
    let mut used = Vec::new();
    for &n in pages {
        let Some(page) = all.get(n.wrapping_sub(1)) else {
            continue;
        };
        let extracted = extract_page(doc, page, n as u32).map_err(|e| e.to_string())?;
        for b in extracted.layout.blocks.iter().filter(|b| !b.protected) {
            let key = cache_key(to, &b.source());
            let apart = again_key(&key);
            if !keys.contains(&apart) {
                continue;
            }
            if let Some(text) = cache.get(&apart) {
                cache.put(&key, &text);
                if !used.contains(&key) {
                    used.push(key);
                }
            }
        }
    }
    Ok(used)
}

fn pages_in(book_pages: usize, pages: &[usize]) -> Vec<usize> {
    if pages.is_empty() {
        (1..=book_pages).collect()
    } else {
        let mut v: Vec<usize> = pages
            .iter()
            .copied()
            .filter(|&p| p >= 1 && p <= book_pages)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn page_text(e: &ExtractedPage) -> String {
    e.layout
        .blocks
        .iter()
        .filter(|b| !b.protected)
        .map(|b| b.plain())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The next blocks to translate, up to about `max_chars` characters, from the pages of `pages` (all pages when
/// empty) that are not in `done`.
#[allow(clippy::too_many_arguments)]
pub fn pending(
    doc: &Document,
    book: &BookInfo,
    done: &BTreeSet<usize>,
    cache: &mut dyn Cache,
    to: &Language,
    pages: &[usize],
    max_chars: usize,
    fresh: Fresh,
) -> Result<Pending, String> {
    let all = doc.pages().map_err(|e| e.to_string())?;
    check_pages(all.len(), pages)?;
    let wanted = pages_in(all.len(), pages);
    let open = |p: &usize| !done.contains(p) || fresh.pages.contains(p);
    let mut out = Pending {
        title: book.title.clone(),
        subject: book.subject.clone(),
        source_language: book.source_language.clone(),
        target_language: to.to_string(),
        instructions: instructions(to),
        context_before: String::new(),
        context_after: String::new(),
        blocks: Vec::new(),
        pages_left: wanted.iter().filter(|p| open(p)).count(),
        pages_done: wanted.iter().filter(|p| !open(p)).count(),
        ready: Vec::new(),
        empty: Vec::new(),
        finished_from_earlier_translations: 0,
        note: String::new(),
    };
    let mut size = 0;
    let mut last_page = 0;
    for &n in wanted.iter().filter(|p| open(p)) {
        let extracted = extract_page(doc, &all[n - 1], n as u32).map_err(|e| e.to_string())?;
        adopt_earlier(cache, to, &extracted.layout.blocks);
        let mut asked = false;
        let mut any = false;
        for b in extracted.layout.blocks.iter().filter(|b| !b.protected) {
            any = true;
            let source = b.source();
            if fresh.known(cache, n, &cache_key(to, &source)) {
                continue;
            }
            asked = true;
            if size > 0 && size + source.len() > max_chars {
                break;
            }
            if out.blocks.is_empty() && n > 1 {
                let before = extract_page(doc, &all[n - 2], n as u32 - 1)
                    .map(|e| page_text(&e))
                    .unwrap_or_default();
                let chars: Vec<char> = before.chars().collect();
                out.context_before = chars[chars.len().saturating_sub(800)..].iter().collect();
            }
            size += source.len();
            last_page = n;
            out.blocks.push(PendingBlock {
                page: n,
                id: b.id.0.clone(),
                // Rows of leader dots are handed out cut short, as they are to a model.
                text: crate::layout::short_leaders(&source),
            });
        }
        if !asked {
            out.ready.push(n);
            if !any {
                out.empty.push(n);
            }
        }
        if size >= max_chars {
            break;
        }
    }
    if last_page > 0 && last_page < all.len() {
        out.context_after = extract_page(doc, &all[last_page], last_page as u32 + 1)
            .map(|e| page_text(&e).chars().take(800).collect())
            .unwrap_or_default();
    }
    // Pages being translated again whose every block has its new translation are written from it now.
    let finished: Vec<usize> = out
        .ready
        .iter()
        .copied()
        .filter(|p| fresh.pages.contains(p))
        .collect();
    put_in_use(doc, &all, cache, to, &finished, fresh.keys)?;
    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submitted {
    pub page: usize,
    pub id: String,
    pub text: String,
}

/// A translation as an assistant sends it: the page may come as a number or as text ("12"), and one that a check
/// flags can be stored anyway on its own, without the whole batch.
#[derive(Debug, Clone, Deserialize)]
pub struct Sent {
    #[serde(deserialize_with = "number_or_text")]
    pub page: usize,
    #[serde(deserialize_with = "text_or_number")]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub accept_anyway: bool,
}

impl Sent {
    fn submitted(&self) -> Submitted {
        Submitted {
            page: self.page,
            id: self.id.clone(),
            text: self.text.clone(),
        }
    }
}

fn number_or_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(|n| n as usize)
            .ok_or_else(|| D::Error::custom("page must be a page number")),
        serde_json::Value::String(s) => s
            .trim()
            .parse()
            .map_err(|_| D::Error::custom(format!("page must be a page number, not {s:?}"))),
        _ => Err(D::Error::custom("page must be a page number")),
    }
}

fn text_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Err(D::Error::custom(
            "id must be the block id from get_text_to_translate",
        )),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Rejected {
    pub page: usize,
    pub id: String,
    pub problems: Vec<String>,
}

/// A block written with its original text, and why.
#[derive(Debug, Clone, Serialize)]
pub struct KeptBlock {
    /// `"<page>:<id>"`.
    pub block: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SubmitResult {
    pub accepted: usize,
    pub rejected: Vec<Rejected>,
    /// Pages whose every block is now translated, written into the output PDF.
    pub completed_pages: Vec<usize>,
    /// Blocks that could not be placed even at the smallest readable size and keep their original text.
    pub blocks_kept_original: usize,
    /// Those blocks, by page and id.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kept_original: Vec<KeptBlock>,
    /// Where the translated PDF is (a path on the desktop, an address on the server).
    pub output: Option<String>,
}

/// Check translations and store the good ones in `cache`. With `accept_anyway`, translations with problems are
/// stored too (after the assistant has looked at them); an empty translation never is. Returns the result so far
/// and the pages that are now complete; the caller writes those with [`render`].
pub fn accept(
    doc: &Document,
    cache: &mut dyn Cache,
    to: &Language,
    items: &[Submitted],
    accept_anyway: bool,
    fresh: Fresh,
) -> Result<Accepted, String> {
    let items: Vec<(&Submitted, bool)> = items.iter().map(|it| (it, accept_anyway)).collect();
    accept_each(doc, cache, to, &items, fresh)
}

/// [`accept`] for translations as an assistant sends them: each may be accepted anyway on its own, and
/// `accept_anyway` still covers the whole batch.
pub fn accept_sent(
    doc: &Document,
    cache: &mut dyn Cache,
    to: &Language,
    items: &[Sent],
    accept_anyway: bool,
    fresh: Fresh,
) -> Result<Accepted, String> {
    let owned: Vec<(Submitted, bool)> = items
        .iter()
        .map(|it| (it.submitted(), it.accept_anyway || accept_anyway))
        .collect();
    let items: Vec<(&Submitted, bool)> = owned.iter().map(|(it, a)| (it, *a)).collect();
    accept_each(doc, cache, to, &items, fresh)
}

fn accept_each(
    doc: &Document,
    cache: &mut dyn Cache,
    to: &Language,
    items: &[(&Submitted, bool)],
    fresh: Fresh,
) -> Result<Accepted, String> {
    let all = doc.pages().map_err(|e| e.to_string())?;
    let mut by_page: BTreeMap<usize, Vec<(&Submitted, bool)>> = BTreeMap::new();
    for &(it, anyway) in items {
        by_page.entry(it.page).or_default().push((it, anyway));
    }
    let mut result = SubmitResult::default();
    let mut complete = Vec::new();
    let mut given = Vec::new();
    let mut keys = fresh.keys.clone();
    for (&n, list) in &by_page {
        let Some(page) = all.get(n.wrapping_sub(1)) else {
            for (it, _) in list {
                result.rejected.push(Rejected {
                    page: n,
                    id: it.id.clone(),
                    problems: vec![format!("no such page: the book has {} pages", all.len())],
                });
            }
            continue;
        };
        let again = fresh.pages.contains(&n);
        let extracted = extract_page(doc, page, n as u32).map_err(|e| e.to_string())?;
        adopt_earlier(cache, to, &extracted.layout.blocks);
        let mut apart = Vec::new();
        for &(it, anyway) in list {
            let Some(block) = extracted.layout.blocks.iter().find(|b| b.id.0 == it.id) else {
                result.rejected.push(Rejected {
                    page: n,
                    id: it.id.clone(),
                    problems: vec!["no such block on this page".into()],
                });
                continue;
            };
            let source = block.source();
            let text = strip_source_glosses(&source, it.text.trim());
            // Nothing in place of the text would blank the block out: never stored, whatever was asked.
            if text.trim().is_empty() {
                result.rejected.push(Rejected {
                    page: n,
                    id: it.id.clone(),
                    problems: vec![
                        "empty translation: send the translated text (accept_anyway does not cover an empty one)"
                            .into(),
                    ],
                });
                continue;
            }
            let problems: Vec<String> = check(&crate::layout::short_leaders(&source), &text)
                .iter()
                .map(describe)
                .collect();
            if !problems.is_empty() && !anyway {
                result.rejected.push(Rejected {
                    page: n,
                    id: it.id.clone(),
                    problems,
                });
                continue;
            }
            let key = cache_key(to, &source);
            if again {
                // Kept apart until the whole page has its new translation.
                let waiting = again_key(&key);
                cache.put(&waiting, &text);
                keys.insert(waiting.clone());
                apart.push(waiting);
            } else {
                cache.put(&key, &text);
            }
            result.accepted += 1;
        }
        let now = Fresh {
            pages: fresh.pages,
            keys: &keys,
        };
        let texts: Vec<String> = extracted
            .layout
            .blocks
            .iter()
            .filter(|b| !b.protected)
            .map(|b| cache_key(to, &b.source()))
            .collect();
        // A page without text is finished when blocks are asked for, not by sending something for it.
        let done = !texts.is_empty() && texts.iter().all(|k| now.known(cache, n, k));
        if done {
            complete.push(n);
            if again {
                for key in put_in_use(doc, &all, cache, to, &[n], &keys)? {
                    keys.insert(key.clone());
                    given.push(key);
                }
            }
        } else {
            given.extend(apart);
        }
    }
    let mut seen = std::collections::HashSet::new();
    given.retain(|k| seen.insert(k.clone()));
    Ok(Accepted {
        result,
        complete,
        fresh_keys: given,
    })
}

/// What [`accept`] took in.
pub struct Accepted {
    pub result: SubmitResult,
    /// Pages whose every block now has a translation that counts.
    pub complete: Vec<usize>,
    /// The keys of the translations given for pages being translated again: the key itself for those now in use
    /// (their page is finished), [`again_key`] for those that wait for the rest of their page.
    pub fresh_keys: Vec<String>,
}

/// A translated book written out: the PDF, and for each page how many blocks kept the original text.
pub struct Rendered {
    pub pdf: Vec<u8>,
    pub kept: BTreeMap<usize, usize>,
    /// Those blocks, page by page.
    pub kept_blocks: BTreeMap<usize, Vec<KeptBlock>>,
}

impl Rendered {
    /// The blocks of `pages` that kept their original text.
    pub fn kept_on(&self, pages: &[usize]) -> Vec<KeptBlock> {
        pages
            .iter()
            .filter_map(|p| self.kept_blocks.get(p))
            .flatten()
            .cloned()
            .collect()
    }
}

fn kept_block(page: usize, issue: &crate::render::Issue) -> KeptBlock {
    use crate::render::Issue;
    let (id, reason) = match issue {
        Issue::DoesNotFit { id } => (
            id,
            "does not fit even at the smallest readable size; a shorter translation would"
                .to_owned(),
        ),
        Issue::MissingGlyphs { id, chars } => {
            (id, format!("no font has these characters: {chars}"))
        }
        Issue::UnknownBlock { id } => (id, "not on the page".to_owned()),
    };
    KeptBlock {
        block: format!("{page}:{}", id.0),
        reason,
    }
}

/// Write the translated PDF with `pages` from the cached translations, and pictures redrawn for the language in
/// place of the originals.
pub fn render(
    doc: &Document,
    fonts: &FontSet,
    cache: &mut dyn Cache,
    to: &Language,
    pages: &BTreeSet<usize>,
    redrawn: &[(ObjRef, Vec<u8>)],
) -> Result<Rendered, String> {
    if fonts.is_empty() {
        return Err("no usable fonts".into());
    }
    let all = doc.pages().map_err(|e| e.to_string())?;
    let mut writer = BookWriter::new(doc, fonts);
    for (image, file) in redrawn {
        writer
            .replace_image(*image, file)
            .map_err(|e| e.to_string())?;
    }
    let mut kept = BTreeMap::new();
    let mut kept_blocks = BTreeMap::new();
    for &n in pages {
        let Some(page) = all.get(n.wrapping_sub(1)) else {
            continue;
        };
        let extracted = extract_page(doc, page, n as u32).map_err(|e| e.to_string())?;
        adopt_earlier(cache, to, &extracted.layout.blocks);
        let translations: BTreeMap<BlockId, String> = extracted
            .layout
            .blocks
            .iter()
            .filter(|b| !b.protected)
            .filter_map(|b| {
                cache
                    .get(&cache_key(to, &b.source()))
                    .map(|t| (b.id.clone(), t))
            })
            .collect();
        let report = writer
            .translate_page(page, &extracted, &translations, to)
            .map_err(|e| e.to_string())?;
        kept.insert(n, report.kept.len());
        if !report.kept.is_empty() {
            kept_blocks.insert(n, report.kept.iter().map(|i| kept_block(n, i)).collect());
        }
    }
    let pdf = writer.finish().map_err(|e| e.to_string())?;
    Ok(Rendered {
        pdf,
        kept,
        kept_blocks,
    })
}

/// Characters to translate on `pages`, for an estimate of time and cost.
pub fn text_size(doc: &Document, pages: &[usize]) -> u64 {
    let Ok(all) = doc.pages() else { return 0 };
    pages
        .iter()
        .filter_map(|&n| {
            let page = all.get(n.wrapping_sub(1))?;
            let e = extract_page(doc, page, n as u32).ok()?;
            Some(
                e.layout
                    .blocks
                    .iter()
                    .filter(|b| !b.protected)
                    .map(|b| b.plain().chars().count() as u64)
                    .sum::<u64>(),
            )
        })
        .sum()
}

/// The text of the first pages, to recognise a book.
pub fn opening_text(doc: &Document, max_chars: usize) -> String {
    let Ok(pages) = doc.pages() else {
        return String::new();
    };
    let mut text = String::new();
    for (i, page) in pages.iter().enumerate().take(14) {
        if let Ok(e) = extract_page(doc, page, i as u32 + 1) {
            for b in &e.layout.blocks {
                text.push_str(&b.plain());
                text.push('\n');
            }
        }
        if text.chars().count() >= max_chars {
            break;
        }
    }
    text.chars().take(max_chars).collect()
}

// ---------- pictures ----------

/// A picture on a page.
#[derive(Debug, Clone, Serialize)]
pub struct Picture {
    /// `"<object> <generation>"`, as the other picture calls take it.
    pub image: String,
    pub page: usize,
    /// Size in pixels.
    pub width: u32,
    pub height: u32,
}

pub fn image_id(id: ObjRef) -> String {
    format!("{} {}", id.num, id.generation)
}

/// Parse `"<object> <generation>"`.
pub fn parse_image_id(s: &str) -> Option<ObjRef> {
    let mut it = s.split_whitespace();
    let num = it.next()?.parse().ok()?;
    let generation = it.next().unwrap_or("0").parse().ok()?;
    it.next().is_none().then_some(ObjRef { num, generation })
}

/// The pictures on `pages` (all pages when empty), largest first within a page; icons and rules are left out.
pub fn pictures(doc: &Document, pages: &[usize]) -> Result<Vec<Picture>, String> {
    let all = doc.pages().map_err(|e| e.to_string())?;
    let wanted: Vec<usize> = if pages.is_empty() {
        (1..=all.len()).collect()
    } else {
        pages.to_vec()
    };
    let mut out = Vec::new();
    for n in wanted {
        let page = all
            .get(n.wrapping_sub(1))
            .ok_or_else(|| format!("The book has no page {n}."))?;
        out.extend(
            celsus_pdf::image::page_images(doc, page)
                .into_iter()
                .map(|i| Picture {
                    image: image_id(i.id),
                    page: n,
                    width: i.width,
                    height: i.height,
                }),
        );
    }
    Ok(out)
}

/// The picture `image` on `page`, or the largest one there when `image` is `None` (on page 1, the cover). Returns
/// its id and its width over height.
pub fn find_picture(
    doc: &Document,
    page: usize,
    image: Option<&str>,
) -> Result<(ObjRef, f32), String> {
    let all = doc.pages().map_err(|e| e.to_string())?;
    let p = all
        .get(page.wrapping_sub(1))
        .ok_or_else(|| format!("The book has no page {page}."))?;
    let pictures = celsus_pdf::image::page_images(doc, p);
    let picture = match image {
        Some(id) => {
            let wanted = parse_image_id(id).ok_or("bad picture id")?;
            pictures
                .into_iter()
                .find(|p| p.id == wanted)
                .ok_or("That picture is not on this page.")?
        }
        None => pictures
            .into_iter()
            .next()
            .ok_or_else(|| format!("Page {page} has no picture to redraw."))?,
    };
    Ok((
        picture.id,
        picture.width as f32 / picture.height.max(1) as f32,
    ))
}

/// What a picture model is asked to do to redraw a picture with its text translated.
pub fn redraw_instructions(
    title: &str,
    author: &str,
    page: usize,
    to: &Language,
    note: Option<&str>,
) -> String {
    let what = if page == 1 {
        "the cover of"
    } else {
        "a picture from"
    };
    let by = if author.is_empty() {
        String::new()
    } else {
        format!(" by {author}")
    };
    let mut text = format!(
        "This is {what} the book \"{title}\"{by}. Redraw this exact image with every piece of visible text \
         translated into {}. Keep everything else identical: the composition, illustration, colours, textures and \
         the lettering style of each text (its typeface character, weight, hand lettering, effects, size and \
         placement). Fit longer translations into the same area by adjusting the lettering, not the picture. Keep \
         people's names, publisher logos, barcodes and ISBNs unchanged. Do not add any text or element that is \
         not in the original.",
        language_name(to),
    );
    if let Some(n) = note.map(str::trim).filter(|n| !n.is_empty()) {
        text.push_str("\n\nAlso: ");
        text.push_str(n);
    }
    text
}

/// Whether a file is a picture Celsus can place (JPEG or PNG).
pub fn placeable(file: &[u8]) -> bool {
    celsus_pdf::image::xobject(file).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_ids_round_trip() {
        let id = ObjRef {
            num: 3806,
            generation: 0,
        };
        assert_eq!(parse_image_id(&image_id(id)), Some(id));
        assert_eq!(
            parse_image_id("12"),
            Some(ObjRef {
                num: 12,
                generation: 0
            })
        );
        assert_eq!(parse_image_id("12 0 7"), None);
        assert_eq!(parse_image_id("x"), None);
    }

    #[test]
    fn only_listed_pages_are_offered() {
        assert_eq!(pages_in(5, &[]), vec![1, 2, 3, 4, 5]);
        assert_eq!(pages_in(5, &[9, 2, 2, 0, 4]), vec![2, 4]);
    }

    #[test]
    fn pages_that_are_not_in_the_book_are_refused() {
        assert!(check_pages(5, &[]).is_ok());
        assert!(check_pages(5, &[1, 5]).is_ok());
        for bad in [&[99][..], &[0], &[0, 1, 2, 3, 4]] {
            let e = check_pages(5, bad).unwrap_err();
            assert!(e.contains("the book has 5 pages"), "{e}");
        }
    }

    #[test]
    fn a_language_name_is_answered_with_its_code() {
        assert_eq!(parse_language(" tr ").unwrap().as_str(), "tr");
        let e = parse_language("turkish").unwrap_err();
        assert!(
            e.contains("code") && e.contains("For Turkish it is tr"),
            "{e}"
        );
        let e = parse_language("Chinese").unwrap_err();
        assert!(e.contains("zh-hans") && e.contains("zh-hant"), "{e}");
        let e = parse_language("Klingonish").unwrap_err();
        assert!(e.contains("zh-hans"), "examples are always given: {e}");
        // A well-formed code that is not a language Celsus knows: its own answer says what is near.
        let e = parse_language("qq").unwrap_err();
        assert!(e.contains("not a language Celsus knows"), "{e}");
    }

    #[test]
    fn a_book_is_not_translated_into_its_own_language() {
        let to = |s: &str| s.parse::<Language>().unwrap();
        assert!(same_language("en", &to("en")));
        assert!(same_language("EN-gb", &to("en")));
        assert!(!same_language("", &to("en")));
        assert!(!same_language("auto", &to("tr")));
        assert!(!same_language("de", &to("tr")));
        assert!(same_language_error("tr", &to("tr")).contains("nothing to translate"));
    }

    /// A PDF whose pages draw these content streams, with Helvetica as /F1.
    fn pdf(contents: &[&str]) -> Document {
        let n = contents.len();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        for (i, content) in contents.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * i
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            bytes.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        Document::load(bytes).unwrap()
    }

    /// The blocks of a page: id and source text.
    fn blocks(doc: &Document, page: usize) -> Vec<(String, String)> {
        let all = doc.pages().unwrap();
        extract_page(doc, &all[page - 1], page as u32)
            .unwrap()
            .layout
            .blocks
            .iter()
            .filter(|b| !b.protected)
            .map(|b| (b.id.0.clone(), b.source()))
            .collect()
    }

    fn sent(page: usize, id: &str, text: &str) -> Submitted {
        Submitted {
            page,
            id: id.into(),
            text: text.into(),
        }
    }

    #[test]
    fn a_page_translated_again_keeps_its_earlier_text_until_all_of_it_is_new() {
        let to: Language = "tr".parse().unwrap();
        let doc = pdf(&[
            "BT /F1 14 Tf 72 700 Td (The opening words of the page) Tj ET BT /F1 14 Tf 72 200 Td (The closing words of the page) Tj ET",
            "BT /F1 14 Tf 72 700 Td (Another page of the same book) Tj ET",
        ]);
        let first = blocks(&doc, 1);
        assert_eq!(first.len(), 2, "{first:?}");
        let mut cache = BTreeMap::new();
        for (_, source) in first.iter().chain(&blocks(&doc, 2)) {
            cache.insert(cache_key(&to, source), format!("Eski: {source}"));
        }
        let redo: BTreeSet<usize> = [1].into();
        let mut keys = std::collections::HashSet::new();
        let fresh = Fresh {
            pages: &redo,
            keys: &keys,
        };
        // Half of the page: kept apart, and the page is written as it was.
        let a = accept(
            &doc,
            &mut cache,
            &to,
            &[sent(1, &first[0].0, "Sayfanın ilk sözleri")],
            false,
            fresh,
        )
        .unwrap();
        assert_eq!(a.result.accepted, 1, "{:?}", a.result.rejected);
        assert!(a.complete.is_empty());
        let old = cache_key(&to, &first[0].1);
        assert_eq!(cache[&old], format!("Eski: {}", first[0].1));
        assert_eq!(a.fresh_keys, vec![again_key(&old)]);
        keys.extend(a.fresh_keys);
        // The other half is still asked for.
        let done: BTreeSet<usize> = [1, 2].into();
        let fresh = Fresh {
            pages: &redo,
            keys: &keys,
        };
        let p = pending(
            &doc,
            &BookInfo::default(),
            &done,
            &mut cache,
            &to,
            &[],
            4000,
            fresh,
        )
        .unwrap();
        assert_eq!(
            p.blocks.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec![first[1].0.as_str()]
        );
        // The rest of it: the whole page takes its new translation at once.
        let a = accept(
            &doc,
            &mut cache,
            &to,
            &[sent(1, &first[1].0, "Sayfanın son sözleri")],
            false,
            fresh,
        )
        .unwrap();
        assert_eq!(a.complete, vec![1]);
        assert_eq!(cache[&old], "Sayfanın ilk sözleri");
        assert_eq!(cache[&cache_key(&to, &first[1].1)], "Sayfanın son sözleri");
    }

    #[test]
    fn empty_translations_are_never_kept_and_flags_are_waived_one_by_one() {
        let to: Language = "tr".parse().unwrap();
        let doc = pdf(&[
            "BT /F1 14 Tf 72 700 Td (Chapter 12 has 345 pages in all) Tj ET BT /F1 14 Tf 72 200 Td (Section 7 adds 89 more of them) Tj ET",
            "0 0 100 100 re f",
        ]);
        let b = blocks(&doc, 1);
        assert_eq!(b.len(), 2, "{b:?}");
        let mut cache = BTreeMap::new();
        // Not even with accept_anyway for the whole batch.
        let a = accept(
            &doc,
            &mut cache,
            &to,
            &[sent(1, &b[0].0, "   "), sent(1, &b[1].0, "")],
            true,
            Fresh::none(),
        )
        .unwrap();
        assert_eq!(a.result.accepted, 0);
        assert!(
            a.result
                .rejected
                .iter()
                .all(|r| r.problems[0].contains("empty"))
        );
        // Both lose their numbers; only the one waived is kept.
        let items = [
            Sent {
                page: 1,
                id: b[0].0.clone(),
                text: "Bölümün bütün sayfaları".into(),
                accept_anyway: true,
            },
            Sent {
                page: 1,
                id: b[1].0.clone(),
                text: "Kısım daha fazlasını ekler".into(),
                accept_anyway: false,
            },
        ];
        let a = accept_sent(&doc, &mut cache, &to, &items, false, Fresh::none()).unwrap();
        assert_eq!(a.result.accepted, 1, "{:?}", a.result.rejected);
        assert_eq!(a.result.rejected.len(), 1);
        assert_eq!(a.result.rejected[0].id, b[1].0);
        assert!(cache.contains_key(&cache_key(&to, &b[0].1)));
        assert!(!cache.contains_key(&cache_key(&to, &b[1].1)));
        // A page without text is not finished by sending something for it.
        let a = accept(
            &doc,
            &mut cache,
            &to,
            &[sent(2, "b0", "Bir şey")],
            true,
            Fresh::none(),
        )
        .unwrap();
        assert!(a.complete.is_empty());
        assert_eq!(a.result.rejected.len(), 1);
    }

    #[test]
    fn pages_may_come_as_text() {
        let items: Vec<Sent> = serde_json::from_str(
            r#"[{"page": "3", "id": "b1", "text": "x"}, {"page": 4, "id": 7, "text": "y", "accept_anyway": true}]"#,
        )
        .unwrap();
        assert_eq!((items[0].page, items[0].accept_anyway), (3, false));
        assert_eq!(
            (items[1].page, items[1].id.as_str(), items[1].accept_anyway),
            (4, "7", true)
        );
        assert!(
            serde_json::from_str::<Vec<Sent>>(r#"[{"page": "three", "id": "b1", "text": "x"}]"#)
                .is_err()
        );
    }
}
