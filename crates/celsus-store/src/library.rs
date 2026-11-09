//! Adding books to the library: copying the file, reading its details and finding a cover.

use std::path::{Path, PathBuf};

use celsus_pdf::{Document, ObjRef, Object};
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
    book_dir(library, id).join(format!("{lang}.pdf"))
}

/// Pictures redrawn in one language, one file per picture, named `p<page>-<object>-<generation>.jpg` (or `.png`).
pub fn redrawn_dir(library: &Path, id: i64, lang: &str) -> PathBuf {
    book_dir(library, id).join(format!("redrawn-{lang}"))
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
    let mut parts = s.split_whitespace();
    let num = parts.next()?.parse().ok()?;
    let generation = parts.next().unwrap_or("0").parse().ok()?;
    Some(ObjRef { num, generation })
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
    let Ok(pages) = doc.pages() else {
        return String::new();
    };
    let mut text = String::new();
    for (i, page) in pages.iter().enumerate().take(14) {
        if let Ok(e) = celsus_core::extract::extract_page(doc, page, i as u32 + 1) {
            for b in &e.layout.blocks {
                text.push_str(&b.source());
                text.push('\n');
            }
        }
        if text.chars().count() >= max_chars {
            break;
        }
    }
    text.chars().take(max_chars).collect()
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

/// The largest JPEG image on the first page, if it is big enough to be a cover. Taken as is: no decoding needed.
fn cover_jpeg(doc: &Document) -> Option<Vec<u8>> {
    let page = doc.pages().ok()?.into_iter().next()?;
    let xobjects = doc.get_in(&page.resources, "XObject")?;
    let mut best: Option<(i64, Vec<u8>)> = None;
    for entry in xobjects.as_dict()?.values() {
        let Ok(obj) = doc.resolve(entry) else {
            continue;
        };
        let Some(stream) = obj.as_stream() else {
            continue;
        };
        let d = &stream.dict;
        let is_image = d.get(&b"Subtype"[..]).and_then(Object::as_name) == Some(b"Image");
        let filter = d.get(&b"Filter"[..]);
        let is_jpeg = match filter {
            Some(Object::Name(n)) => n == b"DCTDecode",
            Some(Object::Array(a)) => a.len() == 1 && a[0].as_name() == Some(b"DCTDecode"),
            _ => false,
        };
        let w = d.get(&b"Width"[..]).and_then(Object::as_i64).unwrap_or(0);
        let h = d.get(&b"Height"[..]).and_then(Object::as_i64).unwrap_or(0);
        if is_image
            && is_jpeg
            && w >= 200
            && h >= 200
            && best.as_ref().is_none_or(|(a, _)| w * h > *a)
        {
            best = Some((w * h, stream.data.clone()));
        }
    }
    best.map(|(_, data)| data)
}

/// Copy a PDF into the library and record it.
pub fn add_book(conn: &Connection, library: &Path, path: &Path) -> Result<Book> {
    let doc = Document::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let pages = doc.pages().map_err(|e| e.to_string())?.len() as i64;
    let original_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = Some(info(&doc, "Title"))
        .filter(|t| {
            t.len() > 2 && !t.to_lowercase().ends_with(".pdf") && !t.contains("Microsoft Word")
        })
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
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::copy(path, source_file(library, book.id)) {
        let _ = db::delete_book(conn, book.id);
        return Err(e.to_string());
    }
    if let Some(jpeg) = cover_jpeg(&doc)
        && std::fs::write(cover_file(library, book.id), jpeg).is_ok()
    {
        db::set_cover(conn, book.id, true)?;
        book.has_cover = true;
    }
    db::book(conn, book.id)
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
    fn pdf_text_strings_decode() {
        assert_eq!(decode_text(b"\xFE\xFF\x00D\x00D\x00D"), "DDD");
        assert_eq!(decode_text(b"Evans"), "Evans");
    }
}
