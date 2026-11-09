//! Pictures with text in them (covers, diagrams, maps), redrawn in the target language.
//!
//! A redrawn picture is kept next to the book (`redrawn-<lang>/`) and placed over the original whenever the
//! translated PDF is written, so translating more pages later keeps it. A picture comes either from the picture
//! model chosen in Celsus (an image-editing model such as OpenAI's) or ready-made from an assistant.

use std::path::{Path, PathBuf};

use celsus_core::Language;
use celsus_core::provider::OpenAiCompatible;
use celsus_pdf::ObjRef;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::{self, Result};
use crate::{assistant, library, secrets};

/// One picture on a page.
#[derive(Debug, Clone, Serialize)]
pub struct Picture {
    /// `"<object> <generation>"`, as the other picture calls take it.
    pub image: String,
    pub page: usize,
    /// Size in pixels.
    pub width: u32,
    pub height: u32,
    /// The redrawn file for the language, if there is one.
    pub redrawn: Option<PathBuf>,
}

fn open(library_dir: &Path, book_id: i64) -> Result<celsus_pdf::Document> {
    celsus_pdf::Document::open(library::source_file(library_dir, book_id))
        .map_err(|e| e.to_string())
}

/// The pictures on `pages` (all pages when empty), largest first within a page.
pub fn list(library_dir: &Path, book_id: i64, lang: &str, pages: &[usize]) -> Result<Vec<Picture>> {
    let doc = open(library_dir, book_id)?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let wanted: Vec<usize> = if pages.is_empty() {
        (1..=all.len()).collect()
    } else {
        pages.to_vec()
    };
    let mut out = Vec::new();
    for n in wanted {
        let Some(page) = all.get(n.wrapping_sub(1)) else {
            return Err(format!("The book has no page {n}."));
        };
        // Largest first; icons and rules are already left out.
        out.extend(
            celsus_pdf::image::page_images(&doc, page)
                .into_iter()
                .map(|i| Picture {
                    image: format!("{} {}", i.id.num, i.id.generation),
                    page: n,
                    width: i.width,
                    height: i.height,
                    redrawn: library::find_redrawn(library_dir, book_id, lang, n, i.id),
                }),
        );
    }
    Ok(out)
}

/// The picture `image` on `page`, or the largest one there when `image` is `None` (on page 1, the cover). Returns
/// its id and its width over height.
fn find(
    library_dir: &Path,
    book_id: i64,
    page: usize,
    image: Option<&str>,
) -> Result<(ObjRef, f32)> {
    let doc = open(library_dir, book_id)?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let p = all
        .get(page.wrapping_sub(1))
        .ok_or_else(|| format!("The book has no page {page}."))?;
    let pictures = celsus_pdf::image::page_images(&doc, p);
    let picture = match image {
        Some(id) => {
            let wanted = library::parse_image_id(id).ok_or("bad picture id")?;
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

/// The original picture as a JPEG or PNG file, with its media type.
pub fn original(
    library_dir: &Path,
    book_id: i64,
    page: usize,
    image: Option<&str>,
) -> Result<(String, Vec<u8>, &'static str)> {
    let (id, _) = find(library_dir, book_id, page, image)?;
    let doc = open(library_dir, book_id)?;
    let (bytes, mime) =
        celsus_pdf::image::export(&doc, id).ok_or("This picture's format cannot be read.")?;
    Ok((format!("{} {}", id.num, id.generation), bytes, mime))
}

/// What the picture model is asked to do.
pub fn instructions(book: &db::Book, page: usize, to: &Language, note: Option<&str>) -> String {
    let what = if page == 1 {
        "the cover of"
    } else {
        "a picture from"
    };
    let by = if book.author.is_empty() {
        String::new()
    } else {
        format!(" by {}", book.author)
    };
    let mut text = format!(
        "This is {what} the book \"{}\"{by}. Redraw this exact image with every piece of visible text translated \
         into {}. Keep everything else identical: the composition, illustration, colours, textures and the \
         lettering style of each text (its typeface character, weight, hand lettering, effects, size and \
         placement). Fit longer translations into the same area by adjusting the lettering, not the picture. Keep \
         people's names, publisher logos, barcodes and ISBNs unchanged. Do not add any text or element that is \
         not in the original.",
        book.title,
        celsus_core::translate::language_name(to),
    );
    if let Some(n) = note.map(str::trim).filter(|n| !n.is_empty()) {
        text.push_str("\n\nAlso: ");
        text.push_str(n);
    }
    text
}

/// A client for the picture model chosen in Settings → Models.
pub fn client(conn: &Connection) -> Result<OpenAiCompatible> {
    let (provider, model) = db::model_for(conn, "image")?.ok_or(
        "No picture model is chosen. In Celsus, open Settings → Models and choose one for pictures (for example \
         OpenAI's gpt-image-1).",
    )?;
    Ok(OpenAiCompatible::new(
        provider.url,
        secrets::get_key(provider.id),
        model,
    ))
}

/// Redraw a picture with the picture model chosen in Celsus and write the translated book with it. Returns the
/// redrawn file.
pub fn redraw(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    page: usize,
    image: Option<&str>,
    note: Option<&str>,
) -> Result<PathBuf> {
    let client = client(conn)?;
    let book = db::book(conn, book_id)?;
    let (id, aspect) = find(library_dir, book_id, page, image)?;
    let doc = open(library_dir, book_id)?;
    let (bytes, mime) =
        celsus_pdf::image::export(&doc, id).ok_or("This picture's format cannot be read.")?;
    let picture = client
        .edit_image(&bytes, mime, aspect, &instructions(&book, page, to, note))
        .map_err(|e| e.to_string())?;
    place(
        conn,
        library_dir,
        book_id,
        to,
        page,
        Some(&format!("{} {}", id.num, id.generation)),
        &picture,
    )
}

/// Put a ready-made picture (JPEG or PNG) in place of the original and write the translated book with it.
pub fn place(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    page: usize,
    image: Option<&str>,
    picture: &[u8],
) -> Result<PathBuf> {
    if celsus_pdf::image::xobject(picture).is_none() {
        return Err("Celsus can place only JPEG and PNG pictures.".into());
    }
    let (id, _) = find(library_dir, book_id, page, image)?;
    let lang = to.as_str();
    library::remove_redrawn(library_dir, book_id, lang, page, id);
    let file = library::redrawn_file(library_dir, book_id, lang, page, id, picture);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, picture).map_err(|e| e.to_string())?;
    assistant::render(conn, library_dir, book_id, to, &[page])?;
    Ok(file)
}

/// Go back to the original picture.
pub fn restore(
    conn: &Connection,
    library_dir: &Path,
    book_id: i64,
    to: &Language,
    page: usize,
    image: Option<&str>,
) -> Result<()> {
    let (id, _) = find(library_dir, book_id, page, image)?;
    library::remove_redrawn(library_dir, book_id, to.as_str(), page, id);
    assistant::render(conn, library_dir, book_id, to, &[])?;
    Ok(())
}
