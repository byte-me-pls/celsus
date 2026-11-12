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
    format!(
        "Translate every block into {to}. Keep the author's meaning, negation, register and tone; write natural, \
         grammatical {to}. Translate short labels from figures and tables too. Keep numbers, formulas, code, \
         identifiers, URLs and proper names exactly as written. Words printed in another style come with tags around \
         them, such as <i>...</i>, <b>...</b>, <code>...</code> or <c>...</c>: keep every tag exactly as written, \
         around the words that say the same in the translation, closed and never one inside another, and add no \
         tags of your own. Do not add notes, explanations, glosses in \
         parentheses or ellipses that are not in the source. Use the book's subject to choose the right sense of a \
         word, and translate recurring terms the same way every time. Send every block back with \
         submit_translations, keeping its page and id. Blocks are independent pieces of the page: translate each \
         one on its own, even when a sentence continues in the next block."
    )
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
) -> Result<Pending, String> {
    let all = doc.pages().map_err(|e| e.to_string())?;
    let wanted = pages_in(all.len(), pages);
    let mut out = Pending {
        title: book.title.clone(),
        subject: book.subject.clone(),
        source_language: book.source_language.clone(),
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
        let extracted = extract_page(doc, &all[n - 1], n as u32).map_err(|e| e.to_string())?;
        adopt_earlier(cache, to, &extracted.layout.blocks);
        for b in extracted.layout.blocks.iter().filter(|b| !b.protected) {
            let source = b.source();
            if cache.get(&cache_key(to, &source)).is_some() {
                continue;
            }
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
        if size >= max_chars {
            break;
        }
    }
    if last_page > 0 && last_page < all.len() {
        out.context_after = extract_page(doc, &all[last_page], last_page as u32 + 1)
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
    /// Where the translated PDF is (a path on the desktop, an address on the server).
    pub output: Option<String>,
}

/// Check translations and store the good ones in `cache`. With `accept_anyway`, translations with problems are
/// stored too (after the assistant has looked at them). Returns the result so far and the pages that are now
/// complete; the caller writes those with [`render`].
pub fn accept(
    doc: &Document,
    cache: &mut dyn Cache,
    to: &Language,
    items: &[Submitted],
    accept_anyway: bool,
) -> Result<(SubmitResult, Vec<usize>), String> {
    let all = doc.pages().map_err(|e| e.to_string())?;
    let mut by_page: BTreeMap<usize, Vec<&Submitted>> = BTreeMap::new();
    for it in items {
        by_page.entry(it.page).or_default().push(it);
    }
    let mut result = SubmitResult::default();
    let mut complete = Vec::new();
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
        let extracted = extract_page(doc, page, n as u32).map_err(|e| e.to_string())?;
        adopt_earlier(cache, to, &extracted.layout.blocks);
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
                check(&crate::layout::short_leaders(&source), &text)
                    .iter()
                    .map(describe)
                    .collect()
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
        let done = extracted
            .layout
            .blocks
            .iter()
            .filter(|b| !b.protected)
            .all(|b| cache.get(&cache_key(to, &b.source())).is_some());
        if done {
            complete.push(n);
        }
    }
    Ok((result, complete))
}

/// A translated book written out: the PDF, and for each page how many blocks kept the original text.
pub struct Rendered {
    pub pdf: Vec<u8>,
    pub kept: BTreeMap<usize, usize>,
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
    }
    let pdf = writer.finish().map_err(|e| e.to_string())?;
    Ok(Rendered { pdf, kept })
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
}
