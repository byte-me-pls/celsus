//! The library work shared by the web interface and the MCP endpoint: handing text to an assistant and taking
//! its translations, writing the translated PDF, and pictures redrawn for a language. The PDF work itself is in
//! [`celsus_core::assist`]; this keeps translations in PostgreSQL and files in the book folders.

use std::collections::{BTreeMap, BTreeSet};

use celsus_core::Language;
use celsus_core::assist::{self, BookInfo, Pending, Picture, SubmitResult, Submitted};
use celsus_core::fonts::FontSet;
use celsus_core::pipeline::Cache;
use tokio::runtime::Handle;

use crate::crypto::sha256;
use crate::web::{ApiError, ApiResult};
use crate::{AppState, library};

/// Translations kept in PostgreSQL, keyed by a hash of language and source text. PDF work runs on a blocking
/// thread, so the cache waits for the database there.
pub struct PgCache {
    pub state: AppState,
    pub handle: Handle,
}

impl Cache for PgCache {
    fn get(&self, key: &str) -> Option<String> {
        self.handle.block_on(async {
            let db = self.state.pool.get().await.ok()?;
            db.query_opt(
                "SELECT text FROM translation_cache WHERE key_hash = $1",
                &[&sha256(key.as_bytes())],
            )
            .await
            .ok()?
            .map(|r| r.get(0))
        })
    }

    fn put(&mut self, key: &str, translation: &str) {
        self.handle.block_on(async {
            if let Ok(db) = self.state.pool.get().await {
                let _ = db
                    .execute(
                        "INSERT INTO translation_cache (key_hash, text) VALUES ($1, $2) ON CONFLICT (key_hash) DO UPDATE SET text = $2",
                        &[&sha256(key.as_bytes()), &translation],
                    )
                    .await;
            }
        });
    }
}

/// A language as the server keeps it in file names and the database: `zh-hans`, not `zh-Hans`.
pub fn code(lang: &Language) -> String {
    lang.as_str().to_lowercase()
}

fn open(state: &AppState, book_id: &str) -> Result<celsus_pdf::Document, String> {
    celsus_pdf::Document::open(library::source_file(&state.config, book_id))
        .map_err(|e| e.to_string())
}

/// Run PDF work off the async threads.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::bad)
}

pub async fn done_pages(state: &AppState, book_id: &str, lang: &str) -> ApiResult<BTreeSet<usize>> {
    let db = state.pool.get().await?;
    Ok(db
        .query(
            "SELECT page FROM translated_pages WHERE book_id = $1 AND lang = $2",
            &[&book_id, &lang],
        )
        .await?
        .iter()
        .map(|r| r.get::<_, i32>(0) as usize)
        .collect())
}

/// The next blocks for an assistant to translate.
pub async fn pending(
    state: &AppState,
    book_id: &str,
    to: &Language,
    pages: Vec<usize>,
    max_chars: usize,
) -> ApiResult<Pending> {
    let db = state.pool.get().await?;
    let row = db
        .query_one(
            "SELECT title, subject, source_lang FROM books WHERE id = $1",
            &[&book_id],
        )
        .await?;
    let info = BookInfo {
        title: row.get(0),
        subject: row.get(1),
        source_language: row.get(2),
    };
    drop(db);
    let done = done_pages(state, book_id, &code(to)).await?;
    let (st, id, to) = (state.clone(), book_id.to_owned(), to.clone());
    let handle = Handle::current();
    blocking(move || {
        let doc = open(&st, &id)?;
        let mut cache = PgCache { state: st, handle };
        assist::pending(&doc, &info, &done, &mut cache, &to, &pages, max_chars)
    })
    .await
}

/// Check and store an assistant's translations; finished pages go into the translated PDF.
pub async fn submit(
    state: &AppState,
    book_id: &str,
    to: &Language,
    items: Vec<Submitted>,
    accept_anyway: bool,
) -> ApiResult<SubmitResult> {
    let (st, id, lang) = (state.clone(), book_id.to_owned(), to.clone());
    let handle = Handle::current();
    let (mut result, complete) = blocking(move || {
        let doc = open(&st, &id)?;
        let mut cache = PgCache { state: st, handle };
        assist::accept(&doc, &mut cache, &lang, &items, accept_anyway)
    })
    .await?;
    if !complete.is_empty() {
        let kept = write_output(state, book_id, to, &complete).await?;
        result.blocks_kept_original = complete.iter().filter_map(|p| kept.get(p)).sum();
        result.completed_pages = complete;
        result.output = Some(format!("/api/books/{book_id}/file/{}", code(to)));
    }
    Ok(result)
}

/// Write the translated PDF again with every finished page plus `new_pages`, and the redrawn pictures. Returns
/// how many blocks kept the original text on each page written. One book's PDF is written by one task at a time.
pub async fn write_output(
    state: &AppState,
    book_id: &str,
    to: &Language,
    new_pages: &[usize],
) -> ApiResult<BTreeMap<usize, usize>> {
    let lang = code(to);
    let _guard = state.outputs.lock(book_id, &lang).await;
    let mut pages = done_pages(state, book_id, &lang).await?;
    pages.extend(new_pages);
    let (st, id, to2) = (state.clone(), book_id.to_owned(), to.clone());
    let handle = Handle::current();
    let lang2 = lang.clone();
    let rendered = blocking(move || {
        let doc = open(&st, &id)?;
        let fonts = FontSet::for_language(&st.config.fonts_dir, &to2);
        let redrawn = library::redrawn_images(&st.config, &id, &lang2);
        let mut cache = PgCache { state: st, handle };
        assist::render(&doc, &fonts, &mut cache, &to2, &pages, &redrawn)
    })
    .await?;
    let output = library::output_file(&state.config, book_id, &lang);
    let tmp = output.with_extension("pdf.part");
    tokio::fs::write(&tmp, &rendered.pdf)
        .await
        .map_err(ApiError::internal)?;
    tokio::fs::rename(&tmp, &output)
        .await
        .map_err(ApiError::internal)?;
    let db = state.pool.get().await?;
    for (&page, &kept) in &rendered.kept {
        db.execute(
            "INSERT INTO translated_pages (book_id, lang, page, kept) VALUES ($1, $2, $3, $4)
             ON CONFLICT (book_id, lang, page) DO UPDATE SET kept = $4",
            &[&book_id, &lang, &(page as i32), &(kept as i32)],
        )
        .await?;
    }
    Ok(rendered.kept)
}

// ---------- pictures ----------

/// A picture with whether it is redrawn in the language.
#[derive(serde::Serialize)]
pub struct PictureState {
    #[serde(flatten)]
    pub picture: Picture,
    pub redrawn: bool,
}

pub async fn pictures(
    state: &AppState,
    book_id: &str,
    lang: Option<&str>,
    pages: Vec<usize>,
) -> ApiResult<Vec<PictureState>> {
    let (st, id) = (state.clone(), book_id.to_owned());
    let list = blocking(move || assist::pictures(&open(&st, &id)?, &pages)).await?;
    Ok(list
        .into_iter()
        .map(|p| {
            let redrawn = lang.is_some_and(|l| {
                assist::parse_image_id(&p.image)
                    .and_then(|i| library::find_redrawn(&state.config, book_id, l, p.page, i))
                    .is_some()
            });
            PictureState {
                picture: p,
                redrawn,
            }
        })
        .collect())
}

/// The original picture as a JPEG or PNG file: its id, bytes and media type.
pub async fn original(
    state: &AppState,
    book_id: &str,
    page: usize,
    image: Option<String>,
) -> ApiResult<(String, Vec<u8>, &'static str)> {
    let (st, id) = (state.clone(), book_id.to_owned());
    blocking(move || {
        let doc = open(&st, &id)?;
        let (obj, _) = assist::find_picture(&doc, page, image.as_deref())?;
        let (bytes, mime) =
            celsus_pdf::image::export(&doc, obj).ok_or("This picture's format cannot be read.")?;
        Ok((assist::image_id(obj), bytes, mime))
    })
    .await
}

/// The redrawn picture in a language.
pub async fn redrawn(
    state: &AppState,
    book_id: &str,
    lang: &str,
    page: usize,
    image: Option<String>,
) -> ApiResult<(String, Vec<u8>, &'static str)> {
    let (st, id) = (state.clone(), book_id.to_owned());
    let (obj, _) =
        blocking(move || assist::find_picture(&open(&st, &id)?, page, image.as_deref())).await?;
    let file = library::find_redrawn(&state.config, book_id, lang, page, obj)
        .ok_or_else(|| ApiError::bad("This picture is not redrawn in that language yet."))?;
    let bytes = tokio::fs::read(file).await.map_err(ApiError::internal)?;
    let mime = if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else {
        "image/jpeg"
    };
    Ok((assist::image_id(obj), bytes, mime))
}

/// Put a picture (JPEG or PNG) in place of the original and write the translated PDF with it.
pub async fn place(
    state: &AppState,
    book_id: &str,
    to: &Language,
    page: usize,
    image: Option<String>,
    picture: Vec<u8>,
) -> ApiResult<String> {
    if !assist::placeable(&picture) {
        return Err(ApiError::bad(
            "Celsus can place only JPEG and PNG pictures.",
        ));
    }
    let (st, id) = (state.clone(), book_id.to_owned());
    let (obj, _) =
        blocking(move || assist::find_picture(&open(&st, &id)?, page, image.as_deref())).await?;
    let lang = code(to);
    library::remove_redrawn(&state.config, book_id, &lang, page, obj);
    let file = library::redrawn_file(&state.config, book_id, &lang, page, obj, &picture);
    if let Some(dir) = file.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(ApiError::internal)?;
    }
    tokio::fs::write(&file, picture)
        .await
        .map_err(ApiError::internal)?;
    write_output(state, book_id, to, &[]).await?;
    Ok(assist::image_id(obj))
}

/// Have the picture model chosen for the server redraw a picture, then place it.
pub async fn redraw(
    state: &AppState,
    book_id: &str,
    to: &Language,
    page: usize,
    image: Option<String>,
    note: Option<String>,
) -> ApiResult<String> {
    let client = crate::worker::model_for(state, "image")
        .await
        .map_err(ApiError::model)?;
    let db = state.pool.get().await?;
    let row = db
        .query_one("SELECT title, author FROM books WHERE id = $1", &[&book_id])
        .await?;
    let (title, author): (String, String) = (row.get(0), row.get(1));
    drop(db);
    let (_, bytes, mime) = original(state, book_id, page, image.clone()).await?;
    let (st, id, img) = (state.clone(), book_id.to_owned(), image.clone());
    let (obj, aspect) =
        blocking(move || assist::find_picture(&open(&st, &id)?, page, img.as_deref())).await?;
    let prompt = assist::redraw_instructions(&title, &author, page, to, note.as_deref());
    let picture = blocking(move || {
        client
            .edit_image(&bytes, mime, aspect, &prompt)
            .map_err(|e| e.to_string())
    })
    .await?;
    place(
        state,
        book_id,
        to,
        page,
        Some(assist::image_id(obj)),
        picture,
    )
    .await
}

/// Go back to the original picture.
pub async fn restore(
    state: &AppState,
    book_id: &str,
    to: &Language,
    page: usize,
    image: Option<String>,
) -> ApiResult<()> {
    let (st, id) = (state.clone(), book_id.to_owned());
    let (obj, _) =
        blocking(move || assist::find_picture(&open(&st, &id)?, page, image.as_deref())).await?;
    library::remove_redrawn(&state.config, book_id, &code(to), page, obj);
    write_output(state, book_id, to, &[]).await?;
    Ok(())
}
