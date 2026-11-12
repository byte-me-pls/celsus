//! Translation by an assistant in a chat (through MCP), on the desktop library. The work itself is in
//! [`celsus_core::assist`]; this keeps the pages and translations in SQLite and the PDFs in the book folders.

use std::collections::BTreeSet;
use std::path::Path;

use celsus_core::Language;
use celsus_core::assist::{self, BookInfo};
use celsus_core::fonts::FontSet;
use rusqlite::Connection;

pub use celsus_core::assist::{Pending, Rejected, SubmitResult, Submitted};

use crate::db::{self, DbCache, Result};
use crate::library;

fn open(library_dir: &Path, book_id: i64) -> Result<celsus_pdf::Document> {
    celsus_pdf::Document::open(library::source_file(library_dir, book_id))
        .map_err(|e| e.to_string())
}

fn done_pages(conn: &Connection, book_id: i64, to: &Language) -> Result<BTreeSet<usize>> {
    Ok(db::translated_pages(conn, book_id, to.as_str())?
        .into_iter()
        .map(|(p, _)| p as usize)
        .collect())
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
    let info = BookInfo {
        title: book.title,
        subject: book.subject,
        source_language: book.source_lang,
    };
    let doc = open(library_dir, book_id)?;
    let done = done_pages(conn, book_id, to)?;
    assist::pending(&doc, &info, &done, &mut DbCache(conn), to, pages, max_chars)
}

/// Check and store translations; pages that are complete are written into the book's output PDF.
pub fn submit(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    items: &[Submitted],
    accept_anyway: bool,
) -> Result<SubmitResult> {
    let doc = open(library_dir, book_id)?;
    let (mut result, complete) =
        assist::accept(&doc, &mut DbCache(conn), to, items, accept_anyway)?;
    if !complete.is_empty() {
        result.blocks_kept_original = render(conn, library_dir, book_id, to, &complete)?;
        result.completed_pages = complete;
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
    let doc = open(library_dir, book_id)?;
    let fonts_dir = db::setting(conn, "fonts_dir")?
        .filter(|d| !d.is_empty())
        .unwrap_or_else(crate::default_fonts_dir);
    let fonts = FontSet::for_language(Path::new(&fonts_dir), to);
    if fonts.is_empty() {
        return Err(format!("no usable fonts in {fonts_dir}"));
    }
    let mut pages = done_pages(conn, book_id, to)?;
    pages.extend(new_pages);
    let redrawn = library::redrawn_images(library_dir, book_id, to.as_str());
    let out = assist::render(&doc, &fonts, &mut DbCache(conn), to, &pages, &redrawn)?;
    for (&n, &kept) in &out.kept {
        db::mark_page(conn, book_id, to.as_str(), n as i64, kept as i64)?;
    }
    std::fs::write(
        library::output_file(library_dir, book_id, to.as_str()),
        out.pdf,
    )
    .map_err(|e| e.to_string())?;
    Ok(new_pages.iter().filter_map(|n| out.kept.get(n)).sum())
}

/// The cache keys that the translated pages of the library are written from. Books that cannot be opened or
/// languages that cannot be read give none: nothing can be written from them either.
pub fn keys_in_use(
    library_dir: &Path,
    translated: &[(i64, String, Vec<usize>)],
) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    for (book, lang, pages) in translated {
        let (Ok(doc), Ok(to)) = (open(library_dir, *book), lang.parse::<Language>()) else {
            continue;
        };
        keys.extend(celsus_core::pipeline::keys_of(&doc, &to, pages));
    }
    keys
}
