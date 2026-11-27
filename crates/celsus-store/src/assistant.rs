//! Translation by an assistant in a chat (through MCP), on the desktop library. The work itself is in
//! [`celsus_core::assist`]; this keeps the pages and translations in SQLite and the PDFs in the book folders.

use std::collections::BTreeSet;
use std::path::Path;

use celsus_core::Language;
use celsus_core::assist::{self, BookInfo, Fresh};
use celsus_core::fonts::FontSet;
use rusqlite::Connection;

pub use celsus_core::assist::{KeptBlock, Pending, Rejected, Sent, SubmitResult, Submitted};

use crate::db::{self, DbCache, Result};
use crate::library;

fn open(library_dir: &Path, book_id: i64) -> Result<celsus_pdf::Document> {
    library::open_book(library_dir, book_id)
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
    let redo = db::redo_pages(conn, book_id, to.as_str())?;
    let keys = db::redo_keys(conn, book_id, to.as_str())?;
    let fresh = Fresh {
        pages: &redo,
        keys: &keys,
    };
    let mut p = assist::pending(
        &doc,
        &info,
        &done,
        &mut DbCache(conn),
        to,
        pages,
        max_chars,
        fresh,
    )?;
    // Pages whose text was all translated before (the same book added again, a repeated page) are written
    // now: nothing is left to ask for them, and they would otherwise never be finished.
    if !p.ready.is_empty() {
        render(conn, library_dir, book_id, to, &p.ready)?;
        db::finish_redo(conn, book_id, to.as_str(), &p.ready)?;
        p.count_ready(&redo);
    }
    Ok(p)
}

/// Translate pages again from the start: earlier translations of their text stop counting for them, and their
/// blocks are handed out again. The pages stay in the translated PDF as they were until their new translation
/// is finished. No pages: the whole book. Returns how many pages are to be translated again.
pub fn translate_again(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    pages: &[usize],
) -> Result<usize> {
    let count = open(library_dir, book_id)?
        .pages()
        .map_err(|e| e.to_string())?
        .len();
    assist::check_pages(count, pages)?;
    let list: Vec<usize> = if pages.is_empty() {
        (1..=count).collect()
    } else {
        pages.to_vec()
    };
    db::add_redo_pages(conn, book_id, to.as_str(), &list)?;
    Ok(list.len())
}

/// Check and store translations; pages that are complete are written into the book's output PDF. Each
/// translation may be accepted anyway on its own; `accept_anyway` accepts the whole batch.
pub fn submit(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    items: &[Sent],
    accept_anyway: bool,
) -> Result<SubmitResult> {
    let doc = open(library_dir, book_id)?;
    let redo = db::redo_pages(conn, book_id, to.as_str())?;
    let keys = db::redo_keys(conn, book_id, to.as_str())?;
    let fresh = Fresh {
        pages: &redo,
        keys: &keys,
    };
    let assist::Accepted {
        mut result,
        complete,
        fresh_keys,
    } = assist::accept_sent(&doc, &mut DbCache(conn), to, items, accept_anyway, fresh)?;
    db::add_redo_keys(conn, book_id, to.as_str(), &fresh_keys)?;
    if !complete.is_empty() {
        db::finish_redo(conn, book_id, to.as_str(), &complete)?;
        result.kept_original = written(conn, library_dir, book_id, to, &complete)?;
        result.blocks_kept_original = result.kept_original.len();
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
    Ok(written(conn, library_dir, book_id, to, new_pages)?.len())
}

/// [`render`], returning the blocks of the new pages that kept their original text.
fn written(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    new_pages: &[usize],
) -> Result<Vec<KeptBlock>> {
    let doc = open(library_dir, book_id)?;
    let fonts_dir = db::setting(conn, "fonts_dir")?
        .filter(|d| !d.is_empty())
        .unwrap_or_else(crate::default_fonts_dir);
    let fonts = FontSet::for_language(Path::new(&fonts_dir), to);
    if fonts.is_empty() {
        return Err(format!("no usable fonts in {fonts_dir}"));
    }
    // One writer at a time, and the finished pages read once it is this one's turn: each write holds every
    // page finished before it.
    let _lock = library::lock_output(library_dir, book_id, to.as_str())?;
    let mut pages = done_pages(conn, book_id, to)?;
    pages.extend(new_pages);
    let redrawn = library::redrawn_images(library_dir, book_id, to.as_str());
    let out = assist::render(&doc, &fonts, &mut DbCache(conn), to, &pages, &redrawn)?;
    // The pages count as finished once they are in the file: if it cannot be written, they are written the
    // next time blocks are asked for.
    library::write_output(
        &library::output_file(library_dir, book_id, to.as_str()),
        &out.pdf,
    )?;
    for (&n, &kept) in &out.kept {
        db::mark_page(conn, book_id, to.as_str(), n as i64, kept as i64)?;
    }
    Ok(out.kept_on(new_pages))
}

/// Remove a book and everything translated for it. Its translations leave the cache too, so that the same
/// file added again is not taken for translated; text that the translated pages of another book also hold
/// stays, or those pages would lose it. `removed` is called once the book is gone from the library, before
/// the slower clearing of the cache. Returns how many kept translations went.
pub fn remove_book(
    db: &std::sync::Mutex<Connection>,
    library_dir: &Path,
    book_id: i64,
    removed: impl FnOnce(),
) -> Result<usize> {
    let langs = db::book_langs(&db.lock().unwrap(), book_id)?;
    // The book's own text is read before its file goes.
    let mut mine = std::collections::HashSet::new();
    if let Ok(doc) = open(library_dir, book_id) {
        let pages: Vec<usize> = (1..=doc.pages().map(|p| p.len()).unwrap_or(0)).collect();
        for to in langs.iter().filter_map(|l| l.parse::<Language>().ok()) {
            mine.extend(with_waiting(celsus_core::pipeline::keys_of(
                &doc, &to, &pages,
            )));
        }
    }
    library::remove_book(&db.lock().unwrap(), library_dir, book_id)?;
    removed();
    if mine.is_empty() {
        return Ok(0);
    }
    let translated = db::all_translated_pages(&db.lock().unwrap())?;
    let keep = keys_in_use(library_dir, &translated);
    db::delete_cache_keys(&db.lock().unwrap(), mine.difference(&keep))
}

/// Remove a book's translation into one language: the translated PDF, its pages, the pictures redrawn for it,
/// its jobs, and the kept translations of the book's text in that language, except those that the translated
/// pages of another book (or of this one in another language) still hold. Returns how many kept translations
/// went. A job translating the book into the language must be stopped before.
pub fn remove_translation(
    db: &std::sync::Mutex<Connection>,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
) -> Result<usize> {
    let mut mine = std::collections::HashSet::new();
    if let Ok(doc) = open(library_dir, book_id) {
        let pages: Vec<usize> = (1..=doc.pages().map(|p| p.len()).unwrap_or(0)).collect();
        mine.extend(with_waiting(celsus_core::pipeline::keys_of(
            &doc, to, &pages,
        )));
    }
    db::delete_translation(&db.lock().unwrap(), book_id, to.as_str())?;
    let file = library::output_file(library_dir, book_id, to.as_str());
    if file.exists() {
        std::fs::remove_file(&file).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(library::redrawn_dir(library_dir, book_id, to.as_str()));
    if mine.is_empty() {
        return Ok(0);
    }
    let translated = db::all_translated_pages(&db.lock().unwrap())?;
    let keep = keys_in_use(library_dir, &translated);
    db::delete_cache_keys(&db.lock().unwrap(), mine.difference(&keep))
}

/// Cache keys, and the keys under which new translations of their text wait while pages are translated again.
fn with_waiting(keys: Vec<String>) -> Vec<String> {
    let waiting: Vec<String> = keys.iter().map(|k| assist::again_key(k)).collect();
    keys.into_iter().chain(waiting).collect()
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
