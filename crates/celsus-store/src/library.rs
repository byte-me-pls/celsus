//! Adding books to the library: copying the file, reading its details and keeping its cover.

use std::path::{Path, PathBuf};

use celsus_pdf::{Document, ObjRef};
use rusqlite::Connection;

use crate::db::{self, Book, Result};

/// Where everything of one book lives.
pub fn book_dir(library: &Path, id: i64) -> PathBuf {
    library.join("books").join(id.to_string())
}

pub fn source_file(library: &Path, id: i64) -> PathBuf {
    book_dir(library, id).join("source.pdf")
}

pub fn cover_file(library: &Path, id: i64) -> PathBuf {
    book_dir(library, id).join("cover.jpg")
}

pub fn output_file(library: &Path, id: i64, lang: &str) -> PathBuf {
    book_dir(library, id).join(format!("{}.pdf", crate::db::lang_key(lang)))
}

/// Give the files of every book the one spelling of their language. Where file names tell upper case from
/// lower, a translation written as "zh-Hans.pdf" would otherwise not be found as "zh-hans.pdf".
pub fn settle_language_names(library: &Path) {
    let Ok(books) = std::fs::read_dir(library.join("books")) else {
        return;
    };
    for book in books.flatten() {
        let Ok(files) = std::fs::read_dir(book.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let named_by_language =
                name.starts_with("redrawn-") || (name.ends_with(".pdf") && name != "source.pdf");
            let lower = name.to_lowercase();
            if !named_by_language || lower == name {
                continue;
            }
            let settled = book.path().join(lower);
            // Where the two names are one file, or the settled one is there already, nothing is moved.
            if !settled.exists() {
                let _ = std::fs::rename(file.path(), settled);
            }
        }
    }
}

/// Pictures redrawn in one language, one file per picture, named `p<page>-<object>-<generation>.jpg` (or `.png`).
pub fn redrawn_dir(library: &Path, id: i64, lang: &str) -> PathBuf {
    book_dir(library, id).join(format!("redrawn-{}", crate::db::lang_key(lang)))
}

/// Where a redrawn picture goes; the extension follows the file's type.
pub fn redrawn_file(
    library: &Path,
    id: i64,
    lang: &str,
    page: usize,
    image: ObjRef,
    bytes: &[u8],
) -> PathBuf {
    let ext = if bytes.starts_with(b"\x89PNG") {
        "png"
    } else {
        "jpg"
    };
    redrawn_dir(library, id, lang).join(format!("p{page}-{}-{}.{ext}", image.num, image.generation))
}

/// The redrawn file of a picture, if there is one.
pub fn find_redrawn(
    library: &Path,
    id: i64,
    lang: &str,
    page: usize,
    image: ObjRef,
) -> Option<PathBuf> {
    ["jpg", "png"]
        .iter()
        .map(|ext| {
            redrawn_dir(library, id, lang)
                .join(format!("p{page}-{}-{}.{ext}", image.num, image.generation))
        })
        .find(|p| p.exists())
}

/// Remove a picture's redrawn files (both types).
pub fn remove_redrawn(library: &Path, id: i64, lang: &str, page: usize, image: ObjRef) {
    while let Some(p) = find_redrawn(library, id, lang, page, image) {
        if std::fs::remove_file(&p).is_err() {
            break;
        }
    }
}

/// A picture redrawn in a language.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Redrawn {
    pub page: usize,
    /// The image XObject it replaces, as `"num gen"`.
    pub image: String,
    pub path: String,
}

pub fn parse_image_id(s: &str) -> Option<ObjRef> {
    celsus_core::assist::parse_image_id(s)
}

pub fn redrawn(library: &Path, id: i64, lang: &str) -> Vec<Redrawn> {
    let mut out: Vec<Redrawn> = std::fs::read_dir(redrawn_dir(library, id, lang))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let stem = name
                        .strip_suffix(".jpg")
                        .or_else(|| name.strip_suffix(".png"))?
                        .strip_prefix('p')?;
                    let mut parts = stem.split('-');
                    let page = parts.next()?.parse().ok()?;
                    let num: u32 = parts.next()?.parse().ok()?;
                    let generation: u16 = parts.next()?.parse().ok()?;
                    Some(Redrawn {
                        page,
                        image: format!("{num} {generation}"),
                        path: e.path().display().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|r| r.page);
    out
}

/// The redrawn pictures of a language, ready for the writer.
pub fn redrawn_images(library: &Path, id: i64, lang: &str) -> Vec<(ObjRef, Vec<u8>)> {
    redrawn(library, id, lang)
        .into_iter()
        .filter_map(|r| Some((parse_image_id(&r.image)?, std::fs::read(&r.path).ok()?)))
        .collect()
}

/// Text of the first pages of a book, for recognising it.
pub fn opening_text(doc: &Document, max_chars: usize) -> String {
    celsus_core::assist::opening_text(doc, max_chars)
}

/// PDF text strings are PDFDocEncoding or UTF-16BE with a byte order mark.
fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

fn info(doc: &Document, key: &str) -> String {
    doc.trailer
        .get(&b"Info"[..])
        .and_then(|i| doc.resolve(i).ok())
        .and_then(|i| i.as_dict().and_then(|d| doc.get_in(d, key)))
        .and_then(|t| t.as_str_bytes().map(decode_text))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// The largest cover file kept. A drawn first page is far smaller.
const COVER_BYTES: usize = 2 << 20;

/// Keep `jpeg` as the book's cover. The app draws the first page for it, so the cover is what a reader sees
/// there, whatever the page is made of.
pub fn set_cover(conn: &Connection, library: &Path, id: i64, jpeg: &[u8]) -> Result<()> {
    db::book(conn, id)?;
    if jpeg.len() > COVER_BYTES || celsus_pdf::image::jpeg_info(jpeg).is_none() {
        return Err("The cover must be a JPEG picture.".into());
    }
    std::fs::write(cover_file(library, id), jpeg).map_err(|e| e.to_string())?;
    db::set_cover(conn, id, true)
}

/// Whether a title from a file's metadata is one a program put there rather than the book's: empty, the file's
/// name, or a word processor's or a layout program's default.
pub fn placeholder_title(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    let t = t.trim_start_matches("microsoft word - ").trim();
    if t.chars().count() <= 2 || t.ends_with(".pdf") || t.ends_with(".doc") || t.ends_with(".docx")
    {
        return true;
    }
    let generic = [
        "untitled",
        "document",
        "layout",
        "powerpoint presentation",
        "presentation",
        "title",
        "book",
        "new document",
        "unknown",
        "adobe indesign",
        "pdf",
    ];
    // "Untitled-1", "Document1", "Layout 1": a default name and a number.
    let word = t.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '-' || c == '_');
    generic.contains(&word) || t.starts_with("microsoft word")
}

/// A book in the library that holds the same file as `path`, byte for byte.
pub fn same_file(conn: &Connection, library: &Path, path: &Path) -> Result<Option<Book>> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len() as i64;
    for book in db::books(conn)? {
        if book.bytes != size {
            continue;
        }
        let (Ok(a), Ok(b)) = (
            std::fs::read(path),
            std::fs::read(source_file(library, book.id)),
        ) else {
            continue;
        };
        if a == b {
            return Ok(Some(book));
        }
    }
    Ok(None)
}

/// Why a file could not be added, in a word the interface has text for: not a PDF, locked with a password, or
/// damaged beyond reading; anything else is "other".
pub fn add_failure(error: &str) -> &'static str {
    let e = error.to_lowercase();
    if e.contains("password") || e.contains("encrypt") {
        "pdf_password"
    } else if e.contains("not a pdf") || e.contains("no %pdf") || e.contains("header") {
        "not_pdf"
    } else if e.contains("damaged")
        || e.contains("catalog")
        || e.contains("xref")
        || e.contains("trailer")
    {
        "pdf_damaged"
    } else {
        "other"
    }
}

/// Copy a PDF into the library and record it.
pub fn add_book(conn: &Connection, library: &Path, path: &Path) -> Result<Book> {
    add_book_checked(conn, library, path).map(|(book, _)| book)
}

/// Copy a PDF into the library and record it, saying too whether the file was damaged and was read as far as
/// it could be (some of its pages may be missing).
pub fn add_book_checked(conn: &Connection, library: &Path, path: &Path) -> Result<(Book, bool)> {
    let doc = Document::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let repaired = doc.was_repaired();
    let pages = doc.pages().map_err(|e| e.to_string())?.len() as i64;
    let original_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = Some(info(&doc, "Title"))
        .filter(|t| !placeholder_title(t))
        .unwrap_or_else(|| {
            original_name
                .trim_end_matches(".pdf")
                .trim_end_matches(".PDF")
                .replace(['_', '-'], " ")
        });
    let bytes = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    let mut book = Book {
        id: 0,
        title,
        author: info(&doc, "Author"),
        subject: String::new(),
        original_name,
        pages,
        bytes,
        source_lang: String::new(),
        has_cover: false,
        added_at: db::now(),
        opened_at: None,
        translated: Default::default(),
        busy: false,
    };
    book.id = db::insert_book(conn, &book)?;
    let dir = book_dir(library, book.id);
    // The number of a book deleted last is given out again. Whatever of that book stayed behind (a folder that
    // could not be removed, a picture redrawn after it was deleted) is not this book's.
    if dir.exists() && std::fs::remove_dir_all(&dir).is_err() {
        let _ = db::delete_book(conn, book.id);
        return Err(format!(
            "{} is left from a deleted book and cannot be cleared; close what has it open and try again",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::copy(path, source_file(library, book.id)) {
        let _ = db::delete_book(conn, book.id);
        return Err(e.to_string());
    }
    Ok((db::book(conn, book.id)?, repaired))
}

pub fn remove_book(conn: &Connection, library: &Path, id: i64) -> Result<()> {
    db::delete_book(conn, id)?;
    let _ = std::fs::remove_dir_all(book_dir(library, id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_library(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("celsus-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn titles_a_program_left_are_not_taken_for_the_book_s() {
        for t in [
            "Untitled",
            "untitled-1",
            "Document1",
            "Layout 1",
            "PowerPoint Presentation",
            "Microsoft Word - notes.docx",
            "book.pdf",
            "",
        ] {
            assert!(placeholder_title(t), "{t:?}");
        }
        for t in [
            "Calculus Made Easy",
            "The Rust Programming Language",
            "Document Engineering",
        ] {
            assert!(!placeholder_title(t), "{t:?}");
        }
    }

    #[test]
    fn why_a_file_could_not_be_added_is_put_in_a_word() {
        assert_eq!(
            add_failure("x.pdf: the PDF needs a password to open"),
            "pdf_password"
        );
        assert_eq!(
            add_failure("x.pdf: damaged file: no document catalog"),
            "pdf_damaged"
        );
        assert_eq!(add_failure("x.pdf: Access is denied."), "other");
    }

    #[test]
    fn redrawn_pictures_are_found_listed_and_removed() {
        let lib = temp_library("redrawn");
        let image = ObjRef {
            num: 3330,
            generation: 0,
        };
        assert!(find_redrawn(&lib, 1, "tr", 1, image).is_none());
        let png = b"\x89PNG rest";
        let file = redrawn_file(&lib, 1, "tr", 1, image, png);
        assert!(file.ends_with("p1-3330-0.png"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, png).unwrap();
        // A second language and an unrelated file do not mix in.
        std::fs::write(redrawn_dir(&lib, 1, "tr").join("notes.txt"), "x").unwrap();
        let list = redrawn(&lib, 1, "tr");
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].page, list[0].image.as_str()), (1, "3330 0"));
        assert!(redrawn(&lib, 1, "de").is_empty());
        let images = redrawn_images(&lib, 1, "tr");
        assert_eq!(images, vec![(image, png.to_vec())]);
        assert_eq!(find_redrawn(&lib, 1, "tr", 1, image), Some(file));
        remove_redrawn(&lib, 1, "tr", 1, image);
        assert!(find_redrawn(&lib, 1, "tr", 1, image).is_none());
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn jpeg_pictures_get_jpg_names_and_ids_parse() {
        let lib = Path::new("lib");
        let f = redrawn_file(
            lib,
            7,
            "de",
            12,
            ObjRef {
                num: 9,
                generation: 2,
            },
            b"\xFF\xD8",
        );
        assert!(f.ends_with("p12-9-2.jpg"));
        assert_eq!(
            parse_image_id("12 0"),
            Some(ObjRef {
                num: 12,
                generation: 0
            })
        );
        assert_eq!(
            parse_image_id("12"),
            Some(ObjRef {
                num: 12,
                generation: 0
            })
        );
        assert_eq!(parse_image_id("x"), None);
    }

    #[test]
    fn a_cover_is_kept_if_it_is_a_jpeg_and_old_ones_are_forgotten_once() {
        let lib = temp_library("cover");
        std::fs::create_dir_all(&lib).unwrap();
        let file = lib.join("celsus.db");
        let conn = db::open(&file).unwrap();
        let book = Book {
            id: 0,
            title: "A Book".into(),
            author: String::new(),
            subject: String::new(),
            original_name: "a.pdf".into(),
            pages: 1,
            bytes: 10,
            source_lang: String::new(),
            has_cover: false,
            added_at: db::now(),
            opened_at: None,
            translated: Default::default(),
            busy: false,
        };
        let id = db::insert_book(&conn, &book).unwrap();
        std::fs::create_dir_all(book_dir(&lib, id)).unwrap();
        assert!(set_cover(&conn, &lib, id, b"\x89PNG not a jpeg").is_err());
        assert!(set_cover(&conn, &lib, id + 1, b"").is_err());
        assert!(!db::book(&conn, id).unwrap().has_cover);
        // The start marker and a frame header: the least that reads as a JPEG.
        let jpeg =
            b"\xFF\xD8\xFF\xC0\x00\x11\x08\x02\x80\x01\xE0\x03\x01\x22\x00\x02\x11\x01\x03\x11\x01";
        set_cover(&conn, &lib, id, jpeg).unwrap();
        assert!(db::book(&conn, id).unwrap().has_cover);
        assert_eq!(std::fs::read(cover_file(&lib, id)).unwrap(), jpeg);
        drop(conn);

        // Opening the library again keeps it: covers are forgotten only when coming from the old kind.
        let conn = db::open(&file).unwrap();
        assert!(db::book(&conn, id).unwrap().has_cover);
        conn.execute_batch("PRAGMA user_version = 0;").unwrap();
        drop(conn);
        let conn = db::open(&file).unwrap();
        assert!(!db::book(&conn, id).unwrap().has_cover);
        drop(conn);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn pdf_text_strings_decode() {
        assert_eq!(decode_text(b"\xFE\xFF\x00D\x00D\x00D"), "DDD");
        assert_eq!(decode_text(b"Evans"), "Evans");
    }
}

/// What was read off the pictures of a book's scanned pages: the lines of each page read (none, where nothing
/// could be read), and the pages found to carry text of their own, so that no page is looked at twice.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Scans {
    #[serde(default)]
    pub read: std::collections::BTreeMap<usize, Vec<ReadLine>>,
    #[serde(default)]
    pub with_text: std::collections::BTreeSet<usize>,
}

/// One line read off a picture, with its box in PDF points.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReadLine {
    pub text: String,
    pub bounds: [f32; 4],
}

fn scans_file(library: &Path, id: i64) -> PathBuf {
    book_dir(library, id).join("read.json")
}

pub fn scans(library: &Path, id: i64) -> Scans {
    std::fs::read(scans_file(library, id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Keep what was found of a book's scanned pages, with what is kept already: the app and an assistant may be
/// looking at the same book, and neither loses what the other found.
pub fn keep_scans(library: &Path, id: i64, scans: &Scans) -> Result<()> {
    let path = scans_file(library, id);
    let part = path.with_extension(format!("{}.part", std::process::id()));
    let mut all = self::scans(library, id);
    all.read
        .extend(scans.read.iter().map(|(k, v)| (*k, v.clone())));
    all.with_text.extend(scans.with_text.iter().copied());
    let bytes = serde_json::to_vec(&all).map_err(|e| e.to_string())?;
    std::fs::write(&part, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &path).map_err(|e| e.to_string())
}

/// Open a book's file, with the lines read off its scanned pages laid on them.
pub fn open_book(library: &Path, id: i64) -> Result<Document> {
    let path = source_file(library, id);
    let doc = Document::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    for (page, lines) in scans(library, id).read {
        doc.set_read_lines(
            page,
            lines
                .into_iter()
                .map(|l| celsus_pdf::interp::ScanLine {
                    text: l.text,
                    bounds: l.bounds,
                })
                .collect(),
        );
    }
    Ok(doc)
}

/// The right to write a book's translated file in one language. Assistants (each in a process of its own) and
/// the app may finish pages of the same book at once; each writes the file with every page finished so far,
/// so two writing at once would leave out the other's page. Held while the pages are read and the file is
/// written; let go when dropped.
pub struct OutputLock(PathBuf);

impl Drop for OutputLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Wait for the right to write the translated file of book `id` in `lang`.
pub fn lock_output(library: &Path, id: i64, lang: &str) -> Result<OutputLock> {
    let path = output_file(library, id, lang).with_extension("pdf.lock");
    let started = std::time::Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(OutputLock(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A lock left by a program that stopped half way is let go of after a while: writing a book
                // takes seconds, not minutes.
                let stale = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > std::time::Duration::from_secs(600));
                if stale {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if started.elapsed() > std::time::Duration::from_secs(300) {
                    return Err(format!(
                        "{} has been being written by another program for five minutes; try again later",
                        output_file(library, id, lang).display()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
}

/// Write a translated book. It is written beside the old file and swapped in, so that a crash cannot leave
/// half a PDF; where the old file is open somewhere and cannot be swapped, it is written over.
pub fn write_output(output: &Path, bytes: &[u8]) -> Result<()> {
    let part = output.with_extension(format!("pdf.{}.part", std::process::id()));
    std::fs::write(&part, bytes).map_err(|e| e.to_string())?;
    if std::fs::rename(&part, output).is_err() {
        let written = std::fs::write(output, bytes);
        let _ = std::fs::remove_file(&part);
        written.map_err(|e| {
            format!(
                "{} could not be written ({e}). If it is open in another program, close it: the finished pages \
                 are written the next time blocks are asked for.",
                output.display()
            )
        })?;
    }
    Ok(())
}
