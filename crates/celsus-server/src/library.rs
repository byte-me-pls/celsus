//! Book files on disk and reading an uploaded PDF.
//!
//! Every path is built from a book id the server made (32 hex characters) and a checked language code, never
//! from a name a client sent.

use std::path::PathBuf;

use crate::Config;

/// A new book id: 128 random bits, so ids cannot be guessed or counted through.
pub fn new_id() -> String {
    crate::crypto::random::<16>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A language code as used in file names: `tr`, `pt-br`, `zh-hant`, `es-419`.
pub fn valid_lang(lang: &str) -> bool {
    (2..=12).contains(&lang.len())
        && lang
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !lang.starts_with('-')
        && !lang.ends_with('-')
        && lang.parse::<celsus_core::Language>().is_ok()
}

pub fn book_dir(config: &Config, id: &str) -> PathBuf {
    assert!(valid_id(id), "book ids are checked before use");
    config.library_dir().join(id)
}

pub fn source_file(config: &Config, id: &str) -> PathBuf {
    book_dir(config, id).join("source.pdf")
}

pub fn cover_file(config: &Config, id: &str) -> PathBuf {
    book_dir(config, id).join("cover")
}

pub fn output_file(config: &Config, id: &str, lang: &str) -> PathBuf {
    assert!(valid_lang(lang), "languages are checked before use");
    book_dir(config, id).join(format!("{lang}.pdf"))
}

/// What an uploaded PDF says about itself.
pub struct Parsed {
    pub title: String,
    pub author: String,
    pub pages: i32,
    /// The largest picture on the first page, as an image file.
    pub cover: Option<Vec<u8>>,
}

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

/// Text that came from a file: no control characters, limited length.
pub fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// Read an uploaded file. Runs on a blocking thread; the PDF library caps how much any stream may expand.
pub fn parse(bytes: Vec<u8>, original_name: &str) -> Result<Parsed, String> {
    if !bytes
        .get(..1024)
        .unwrap_or(&bytes)
        .windows(5)
        .any(|w| w == b"%PDF-")
    {
        return Err("This is not a PDF file.".into());
    }
    let doc =
        celsus_pdf::Document::load(bytes).map_err(|e| format!("The PDF could not be read: {e}"))?;
    let pages = doc
        .pages()
        .map_err(|e| format!("The PDF could not be read: {e}"))?;
    if pages.is_empty() {
        return Err("The PDF has no pages.".into());
    }
    let info = |key: &str| {
        doc.trailer
            .get(&b"Info"[..])
            .and_then(|i| doc.resolve(i).ok())
            .and_then(|i| i.as_dict().and_then(|d| doc.get_in(d, key)))
            .and_then(|t| t.as_str_bytes().map(decode_text))
            .map(|s| clean(&s, 300))
            .unwrap_or_default()
    };
    let stem = original_name
        .trim_end_matches(".pdf")
        .trim_end_matches(".PDF")
        .replace(['_', '-'], " ");
    let title = Some(info("Title"))
        .filter(|t| {
            t.len() > 2 && !t.to_lowercase().ends_with(".pdf") && !t.contains("Microsoft Word")
        })
        .unwrap_or_else(|| clean(&stem, 300));
    let cover = celsus_pdf::image::page_images(&doc, &pages[0])
        .into_iter()
        .find(|i| i.width >= 200 && i.height >= 200)
        .and_then(|i| celsus_pdf::image::export(&doc, i.id))
        .map(|(b, _)| b);
    Ok(Parsed {
        title: if title.is_empty() {
            "Untitled".into()
        } else {
            title
        },
        author: info("Author"),
        pages: pages.len().min(i32::MAX as usize) as i32,
        cover,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_languages_are_strict() {
        assert!(valid_id(&new_id()));
        for bad in [
            "",
            "..",
            "../server.key",
            "0123456789abcdef0123456789ABCDEF",
            "0123456789abcdef0123456789abcde/",
            "0123456789abcdef0123456789abcdef0",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(valid_lang("tr") && valid_lang("pt-br"));
        for bad in [
            "",
            "t",
            "TR",
            "../x",
            "tr/",
            "tr.pdf",
            "-tr",
            "a\u{0}b",
            "averyveryverylonglang",
        ] {
            assert!(!valid_lang(bad), "{bad}");
        }
    }

    #[test]
    fn junk_is_not_a_pdf() {
        assert!(parse(b"MZ\x90\x00 not a pdf".to_vec(), "x.pdf").is_err());
        assert!(parse(b"%PDF-1.4 but broken".to_vec(), "x.pdf").is_err());
        assert_eq!(clean("  a\u{0}b\u{7}c  ", 10), "abc");
    }
}
