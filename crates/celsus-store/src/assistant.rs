//! Translation by an assistant in a chat (through MCP): Celsus hands out the text block by block, the assistant
//! sends the translations back, and Celsus checks them and writes the pages. No API key is involved.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use celsus_core::extract::{ExtractedPage, extract_page};
use celsus_core::fonts::FontSet;
use celsus_core::pipeline::{Cache, cache_key};
use celsus_core::render::BookWriter;
use celsus_core::translate::{check, describe, language_name, strip_source_glosses};
use celsus_core::{BlockId, Language};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::{self, DbCache, Result};
use crate::library;

/// What the assistant is told with every batch.
pub fn instructions(to: &Language) -> String {
    let to = language_name(to);
    format!(
        "Translate every block into {to}. Keep the author's meaning, negation, register and tone; write natural, \
         grammatical {to}. Translate short labels from figures and tables too. Keep numbers, formulas, code, \
         identifiers, URLs and proper names exactly as written. Do not add notes, explanations, glosses in \
         parentheses or ellipses that are not in the source. Use the book's subject to choose the right sense of a \
         word, and translate recurring terms the same way every time. Send every block back with \
         submit_translations, keeping its page and id. Blocks are independent pieces of the page: translate each \
         one on its own, even when a sentence continues in the next block."
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingBlock {
    pub page: usize,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pending {
    pub book_id: i64,
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
        .map(|b| b.source())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The next blocks to translate, up to about `max_chars` characters, from the unfinished pages of `pages`.
pub fn pending(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    pages: &[usize],
    max_chars: usize,
) -> Result<Pending> {
    let book = db::book(conn, book_id)?;
    let doc = celsus_pdf::Document::open(library::source_file(library_dir, book_id))
        .map_err(|e| e.to_string())?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let wanted = pages_in(all.len(), pages);
    let done: BTreeSet<usize> = db::translated_pages(conn, book_id, to.as_str())?
        .into_iter()
        .map(|(p, _)| p as usize)
        .collect();
    let mut cache = DbCache(conn);
    let mut out = Pending {
        book_id,
        title: book.title,
        subject: book.subject,
        source_language: book.source_lang,
        target_language: to.to_string(),
        instructions: instructions(to),
        context_before: String::new(),
        context_after: String::new(),
        blocks: Vec::new(),
        pages_left: wanted.iter().filter(|p| !done.contains(p)).count(),
        pages_done: wanted.iter().filter(|p| done.contains(p)).count(),
    };
    let mut size = 0;
    let mut last_page = 0;
    for &n in wanted.iter().filter(|p| !done.contains(p)) {
        let extracted = extract_page(&doc, &all[n - 1], n as u32).map_err(|e| e.to_string())?;
        celsus_core::pipeline::adopt_earlier(&mut cache, to, &extracted.layout.blocks);
        for b in extracted.layout.blocks.iter().filter(|b| !b.protected) {
            let source = b.source();
            if cache.get(&cache_key(to, &source)).is_some() {
                continue;
            }
            if size > 0 && size + source.len() > max_chars {
                break;
            }
            if out.blocks.is_empty() && n > 1 {
                let before = extract_page(&doc, &all[n - 2], n as u32 - 1)
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
                text: source,
            });
        }
        if size >= max_chars {
            break;
        }
    }
    if last_page > 0 && last_page < all.len() {
        out.context_after = extract_page(&doc, &all[last_page], last_page as u32 + 1)
            .map(|e| page_text(&e).chars().take(800).collect())
            .unwrap_or_default();
    }
    Ok(out)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submitted {
    pub page: usize,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rejected {
    pub page: usize,
    pub id: String,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SubmitResult {
    pub accepted: usize,
    pub rejected: Vec<Rejected>,
    /// Pages whose every block is now translated, written into the output PDF.
    pub completed_pages: Vec<usize>,
    /// Blocks that could not be placed even at the smallest readable size and keep their original text.
    pub blocks_kept_original: usize,
    pub output: Option<String>,
}

/// Check and store translations; pages that are complete are written into the book's output PDF.
/// With `accept_anyway`, translations with problems are stored too (after the assistant has looked at them).
pub fn submit(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    items: &[Submitted],
    accept_anyway: bool,
) -> Result<SubmitResult> {
    let doc = celsus_pdf::Document::open(library::source_file(library_dir, book_id))
        .map_err(|e| e.to_string())?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let mut by_page: BTreeMap<usize, Vec<&Submitted>> = BTreeMap::new();
    for it in items {
        by_page.entry(it.page).or_default().push(it);
    }
    let mut result = SubmitResult::default();
    let mut cache = DbCache(conn);
    let mut touched = Vec::new();
    for (&n, list) in &by_page {
        let Some(page) = all.get(n.wrapping_sub(1)) else {
            for it in list {
                result.rejected.push(Rejected {
                    page: n,
                    id: it.id.clone(),
                    problems: vec!["no such page".into()],
                });
            }
            continue;
        };
        let extracted = extract_page(&doc, page, n as u32).map_err(|e| e.to_string())?;
        celsus_core::pipeline::adopt_earlier(&mut cache, to, &extracted.layout.blocks);
        for it in list {
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
            let problems: Vec<String> = if text.is_empty() {
                vec!["empty translation".into()]
            } else {
                check(&source, &text).iter().map(describe).collect()
            };
            if !problems.is_empty() && !accept_anyway {
                result.rejected.push(Rejected {
                    page: n,
                    id: it.id.clone(),
                    problems,
                });
                continue;
            }
            cache.put(&cache_key(to, &source), &text);
            result.accepted += 1;
        }
        let complete = extracted
            .layout
            .blocks
            .iter()
            .filter(|b| !b.protected)
            .all(|b| cache.get(&cache_key(to, &b.source())).is_some());
        if complete {
            touched.push(n);
        }
    }
    if !touched.is_empty() {
        let kept = render(conn, library_dir, book_id, to, &touched)?;
        result.completed_pages = touched;
        result.blocks_kept_original = kept;
        result.output = Some(
            library::output_file(library_dir, book_id, to.as_str())
                .display()
                .to_string(),
        );
    }
    Ok(result)
}

/// Rewrite the output PDF for `to` with every finished page plus `new_pages`, using the cached translations.
/// Returns how many blocks of the new pages kept their original text.
pub fn render(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    new_pages: &[usize],
) -> Result<usize> {
    let doc = celsus_pdf::Document::open(library::source_file(library_dir, book_id))
        .map_err(|e| e.to_string())?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let fonts_dir = db::setting(conn, "fonts_dir")?
        .filter(|d| !d.is_empty())
        .unwrap_or_else(crate::default_fonts_dir);
    let fonts = FontSet::for_language(Path::new(&fonts_dir), to);
    if fonts.is_empty() {
        return Err(format!("no usable fonts in {fonts_dir}"));
    }
    let mut pages: BTreeSet<usize> = db::translated_pages(conn, book_id, to.as_str())?
        .into_iter()
        .map(|(p, _)| p as usize)
        .collect();
    pages.extend(new_pages);
    let mut cache = DbCache(conn);
    let mut writer = BookWriter::new(&doc, &fonts);
    for (image, jpeg) in library::redrawn_images(library_dir, book_id, to.as_str()) {
        writer
            .replace_image(image, &jpeg)
            .map_err(|e| e.to_string())?;
    }
    let mut kept_new = 0;
    for n in pages {
        let Some(page) = all.get(n - 1) else { continue };
        let extracted = extract_page(&doc, page, n as u32).map_err(|e| e.to_string())?;
        celsus_core::pipeline::adopt_earlier(&mut cache, to, &extracted.layout.blocks);
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
        db::mark_page(
            conn,
            book_id,
            to.as_str(),
            n as i64,
            report.kept.len() as i64,
        )?;
        if new_pages.contains(&n) {
            kept_new += report.kept.len();
        }
    }
    let bytes = writer.finish().map_err(|e| e.to_string())?;
    std::fs::write(
        library::output_file(library_dir, book_id, to.as_str()),
        bytes,
    )
    .map_err(|e| e.to_string())?;
    Ok(kept_new)
}
