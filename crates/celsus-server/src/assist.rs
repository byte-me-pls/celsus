//! The library work shared by the web interface and the MCP endpoint: handing text to an assistant and taking
//! its translations, writing the translated PDF, translating pages again, removing a translation, and pictures
//! redrawn for a language. The PDF work itself is in [`celsus_core::assist`]; this keeps translations in
//! PostgreSQL and files in the book folders.

use std::collections::{BTreeSet, HashSet};

use celsus_core::Language;
use celsus_core::assist::{self, BookInfo, Fresh, KeptBlock, Pending, Picture, Sent, SubmitResult};
use celsus_core::fonts::FontSet;
use celsus_core::pipeline::Cache;
use tokio::runtime::Handle;

use crate::crypto::sha256;
use crate::web::{ApiError, ApiResult};
use crate::{AppState, library};

/// One book's translations kept in PostgreSQL, keyed by a hash of the book, the language and the source text.
/// PDF work runs on a blocking thread, so the cache waits for the database there.
///
/// The cache belongs to the book: what someone submits for their own book is never read back into another
/// person's. Entries written before books had their own (no book recorded) are still read, never written.
pub struct PgCache {
    pub state: AppState,
    pub handle: Handle,
    pub book_id: String,
}

impl PgCache {
    /// The hash a key of this book's cache is kept under.
    pub fn scoped(&self, key: &str) -> Vec<u8> {
        scoped(&self.book_id, key)
    }
}

fn scoped(book_id: &str, key: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(book_id.len() + 1 + key.len());
    bytes.extend_from_slice(book_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(key.as_bytes());
    sha256(&bytes)
}

impl Cache for PgCache {
    fn get(&self, key: &str) -> Option<String> {
        self.handle.block_on(async {
            let db = self.state.pool.get().await.ok()?;
            let own = db
                .query_opt(
                    "SELECT text FROM translation_cache WHERE key_hash = $1 AND book_id = $2",
                    &[&self.scoped(key).as_slice(), &self.book_id],
                )
                .await
                .ok()?;
            if let Some(row) = own {
                return Some(row.get(0));
            }
            db.query_opt(
                "SELECT text FROM translation_cache WHERE key_hash = $1 AND book_id IS NULL",
                &[&sha256(key.as_bytes()).as_slice()],
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
                        "INSERT INTO translation_cache (key_hash, text, book_id) VALUES ($1, $2, $3) \
                         ON CONFLICT (key_hash) DO UPDATE SET text = $2 WHERE translation_cache.book_id = $3",
                        &[&self.scoped(key).as_slice(), &translation, &self.book_id],
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

// ---------- pages translated again ----------

/// Pages of a book being translated again into a language: earlier translations of their text do not count.
pub async fn redo_pages(state: &AppState, book_id: &str, lang: &str) -> ApiResult<BTreeSet<usize>> {
    let db = state.pool.get().await?;
    Ok(db
        .query(
            "SELECT page FROM redo_pages WHERE book_id = $1 AND lang = $2",
            &[&book_id, &lang],
        )
        .await?
        .iter()
        .map(|r| r.get::<_, i32>(0) as usize)
        .collect())
}

/// The translations given since the pages of a book began to be translated again (see [`Fresh`]).
pub async fn redo_keys(state: &AppState, book_id: &str, lang: &str) -> ApiResult<HashSet<String>> {
    let db = state.pool.get().await?;
    Ok(db
        .query(
            "SELECT key FROM redo_keys WHERE book_id = $1 AND lang = $2",
            &[&book_id, &lang],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

pub async fn add_redo_pages(
    state: &AppState,
    book_id: &str,
    lang: &str,
    pages: &[usize],
) -> ApiResult<()> {
    let db = state.pool.get().await?;
    let pages: Vec<i32> = pages.iter().map(|&p| p as i32).collect();
    db.execute(
        "INSERT INTO redo_pages (book_id, lang, page) SELECT $1, $2, unnest($3::INT[]) ON CONFLICT DO NOTHING",
        &[&book_id, &lang, &pages],
    )
    .await?;
    Ok(())
}

pub async fn add_redo_keys(
    state: &AppState,
    book_id: &str,
    lang: &str,
    keys: &[String],
) -> ApiResult<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let db = state.pool.get().await?;
    for key in keys {
        db.execute(
            "INSERT INTO redo_keys (book_id, lang, key_hash, key) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            &[&book_id, &lang, &sha256(key.as_bytes()), key],
        )
        .await?;
    }
    Ok(())
}

/// Pages that were translated again to the end. When none is left, the record of what was given goes too.
pub async fn finish_redo(
    state: &AppState,
    book_id: &str,
    lang: &str,
    pages: &[usize],
) -> ApiResult<()> {
    let db = state.pool.get().await?;
    let pages: Vec<i32> = pages.iter().map(|&p| p as i32).collect();
    db.execute(
        "DELETE FROM redo_pages WHERE book_id = $1 AND lang = $2 AND page = ANY($3)",
        &[&book_id, &lang, &pages],
    )
    .await?;
    db.execute(
        "DELETE FROM redo_keys WHERE book_id = $1 AND lang = $2
         AND NOT EXISTS (SELECT 1 FROM redo_pages WHERE book_id = $1 AND lang = $2)",
        &[&book_id, &lang],
    )
    .await?;
    Ok(())
}

/// Translate pages of a book again from the start (all pages when none are given): earlier translations of
/// their text stop counting for them, and they stay in the translated PDF as they were until their new
/// translation is finished. Returns how many pages are to be translated again.
pub async fn translate_again(
    state: &AppState,
    book_id: &str,
    to: &Language,
    pages: &[usize],
) -> ApiResult<usize> {
    let db = state.pool.get().await?;
    let count: i32 = db
        .query_one("SELECT pages FROM books WHERE id = $1", &[&book_id])
        .await?
        .get(0);
    drop(db);
    let count = count.max(0) as usize;
    assist::check_pages(count, pages).map_err(ApiError::bad)?;
    let list: Vec<usize> = if pages.is_empty() {
        (1..=count).collect()
    } else {
        pages.to_vec()
    };
    add_redo_pages(state, book_id, &code(to), &list).await?;
    Ok(list.len())
}

// ---------- assistants ----------

/// The next blocks for an assistant to translate, and the pages of the request that are scans without text.
/// The server cannot read the words off a scan, so those pages are left unfinished rather than written as they
/// are and called translated.
pub async fn pending(
    state: &AppState,
    book_id: &str,
    to: &Language,
    pages: Vec<usize>,
    max_chars: usize,
) -> ApiResult<(Pending, Vec<usize>)> {
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
    let lang = code(to);
    let done = done_pages(state, book_id, &lang).await?;
    let redo = redo_pages(state, book_id, &lang).await?;
    let keys = redo_keys(state, book_id, &lang).await?;
    let again = redo.clone();
    let (st, id, to2) = (state.clone(), book_id.to_owned(), to.clone());
    let handle = Handle::current();
    let (mut p, scans) = blocking(move || {
        let doc = open(&st, &id)?;
        let mut cache = PgCache {
            state: st,
            handle,
            book_id: id,
        };
        let fresh = Fresh {
            pages: &redo,
            keys: &keys,
        };
        let mut p = assist::pending(
            &doc, &info, &done, &mut cache, &to2, &pages, max_chars, fresh,
        )?;
        let all = doc.pages().map_err(|e| e.to_string())?;
        let scans: Vec<usize> = p
            .empty
            .iter()
            .copied()
            .filter(|&n| {
                all.get(n.wrapping_sub(1))
                    .is_some_and(|page| celsus_core::extract::picture_only(&doc, page))
            })
            .collect();
        p.ready.retain(|n| !scans.contains(n));
        p.empty.retain(|n| !scans.contains(n));
        Ok((p, scans))
    })
    .await?;
    // Pages whose text was all translated before (the same book added again, a repeated page) are written
    // now: nothing is left to ask for them, and they would otherwise never be finished.
    if !p.ready.is_empty() {
        write_output(state, book_id, to, &p.ready).await?;
        finish_redo(state, book_id, &lang, &p.ready).await?;
        p.count_ready(&again);
    }
    if !scans.is_empty() {
        let list: Vec<String> = scans.iter().map(usize::to_string).collect();
        let note = format!(
            "Page(s) {} are scans: pictures of a page without a text layer. This server cannot read the text \
             off a scanned page, so they stay unfinished and are not written into the translated PDF; there is \
             nothing on them to hand out. The Celsus desktop app can read scanned pages: translate them there, \
             or leave them out of the pages you ask for.",
            list.join(", ")
        );
        p.note = if p.note.is_empty() {
            note
        } else {
            format!("{} {note}", p.note)
        };
    }
    Ok((p, scans))
}

/// Check and store an assistant's translations; finished pages go into the translated PDF. Each translation
/// may be accepted anyway on its own; `accept_anyway` accepts the whole batch.
pub async fn submit(
    state: &AppState,
    book_id: &str,
    to: &Language,
    items: Vec<Sent>,
    accept_anyway: bool,
) -> ApiResult<SubmitResult> {
    let lang = code(to);
    let redo = redo_pages(state, book_id, &lang).await?;
    let keys = redo_keys(state, book_id, &lang).await?;
    let (st, id, to2) = (state.clone(), book_id.to_owned(), to.clone());
    let handle = Handle::current();
    let assist::Accepted {
        mut result,
        complete,
        fresh_keys,
    } = blocking(move || {
        let doc = open(&st, &id)?;
        let mut cache = PgCache {
            state: st,
            handle,
            book_id: id,
        };
        let fresh = Fresh {
            pages: &redo,
            keys: &keys,
        };
        assist::accept_sent(&doc, &mut cache, &to2, &items, accept_anyway, fresh)
    })
    .await?;
    add_redo_keys(state, book_id, &lang, &fresh_keys).await?;
    if !complete.is_empty() {
        finish_redo(state, book_id, &lang, &complete).await?;
        result.kept_original = write_output(state, book_id, to, &complete).await?;
        result.blocks_kept_original = result.kept_original.len();
        result.completed_pages = complete;
        result.output = Some(format!("/api/books/{book_id}/file/{lang}"));
    }
    Ok(result)
}

/// Write the translated PDF again with every finished page plus `new_pages`, and the redrawn pictures. Returns
/// the blocks of `new_pages` that kept their original text. One book's PDF is written by one task at a time.
pub async fn write_output(
    state: &AppState,
    book_id: &str,
    to: &Language,
    new_pages: &[usize],
) -> ApiResult<Vec<KeptBlock>> {
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
        let mut cache = PgCache {
            state: st,
            handle,
            book_id: id,
        };
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
    Ok(rendered.kept_on(new_pages))
}

// ---------- removing a translation ----------

/// Remove a book's translation into one language: the translated PDF, its pages, the pictures redrawn for it,
/// its jobs, the pages being translated again, and the kept translations of the book's text in that language.
/// The cache belongs to the book, so no other book's pages are written from those; entries from before books
/// had their own cache are shared and stay. A running job on it is stopped and waited for first, so that it
/// cannot write its pages back once they are gone. Returns how many kept translations went.
pub async fn remove_translation(state: &AppState, book_id: &str, to: &Language) -> ApiResult<u64> {
    let lang = code(to);
    let db = state.pool.get().await?;
    let running: Vec<String> = db
        .query(
            "SELECT id FROM jobs WHERE book_id = $1 AND lang = $2 AND status = 'running'",
            &[&book_id, &lang],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    drop(db);
    for job in &running {
        if !state.jobs.cancel(job) {
            continue;
        }
        let mut stopped = false;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let db = state.pool.get().await?;
            let status: String = db
                .query_one("SELECT status FROM jobs WHERE id = $1", &[job])
                .await?
                .get(0);
            if status != "running" {
                stopped = true;
                break;
            }
        }
        if !stopped {
            return Err(ApiError::bad(
                "A job translating this book into that language is still stopping. Try again in a moment.",
            ));
        }
    }
    // The book's own text in that language, read before anything goes.
    let (st, id, to2) = (state.clone(), book_id.to_owned(), to.clone());
    let keys: Vec<Vec<u8>> = blocking(move || {
        let Ok(doc) = open(&st, &id) else {
            return Ok(Vec::new());
        };
        let pages: Vec<usize> = (1..=doc.pages().map(|p| p.len()).unwrap_or(0)).collect();
        let keys = celsus_core::pipeline::keys_of(&doc, &to2, &pages);
        let waiting: Vec<String> = keys.iter().map(|k| assist::again_key(k)).collect();
        Ok(keys
            .iter()
            .chain(&waiting)
            .map(|k| scoped(&id, k))
            .collect())
    })
    .await?;
    let _guard = state.outputs.lock(book_id, &lang).await;
    let mut db = state.pool.get().await?;
    let tx = db.transaction().await?;
    for table in ["translated_pages", "redo_pages", "redo_keys"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE book_id = $1 AND lang = $2"),
            &[&book_id, &lang],
        )
        .await?;
    }
    tx.execute(
        "DELETE FROM jobs WHERE book_id = $1 AND lang = $2 AND status <> 'running'",
        &[&book_id, &lang],
    )
    .await?;
    let gone = tx
        .execute(
            "DELETE FROM translation_cache WHERE book_id = $1 AND key_hash = ANY($2)",
            &[&book_id, &keys],
        )
        .await?;
    tx.commit().await?;
    let file = library::output_file(&state.config, book_id, &lang);
    if let Err(e) = tokio::fs::remove_file(&file).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(ApiError::internal(e));
    }
    let _ = tokio::fs::remove_dir_all(library::redrawn_dir(&state.config, book_id, &lang)).await;
    Ok(gone)
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
    let picture = tokio::task::spawn_blocking(move || client.edit_image(&bytes, mime, aspect, &prompt))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| {
            // The interface has words of its own for an account without credit.
            if celsus_core::provider::is_out_of_credit(&e) {
                ApiError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    celsus_core::provider::OUT_OF_CREDIT,
                    "The picture model's service says the account has no credit left. Add credit there, or \
                     an admin can choose another model under Connections and models.",
                )
            } else {
                ApiError::bad(e.to_string())
            }
        })?;
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
