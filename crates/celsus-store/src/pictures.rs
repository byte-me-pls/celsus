//! Pictures with text in them (covers, diagrams, maps), redrawn in the target language.
//!
//! A redrawn picture is kept next to the book (`redrawn-<lang>/`) and placed over the original whenever the
//! translated PDF is written, so translating more pages later keeps it. A picture comes either from the picture
//! model chosen in Celsus (an image-editing model such as OpenAI's) or ready-made from an assistant.

use std::path::{Path, PathBuf};

use celsus_core::Language;
use celsus_core::assist;
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
    Ok(assist::pictures(&doc, pages)?
        .into_iter()
        .map(|p| {
            let id = assist::parse_image_id(&p.image).expect("ids come from the document");
            Picture {
                redrawn: library::find_redrawn(library_dir, book_id, lang, p.page, id),
                image: p.image,
                page: p.page,
                width: p.width,
                height: p.height,
            }
        })
        .collect())
}

fn find(
    library_dir: &Path,
    book_id: i64,
    page: usize,
    image: Option<&str>,
) -> Result<(ObjRef, f32)> {
    assist::find_picture(&open(library_dir, book_id)?, page, image)
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
    assist::redraw_instructions(&book.title, &book.author, page, to, note)
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
    if !assist::placeable(picture) {
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
