//! The translation queue: one job at a time (a Raspberry Pi has little to spare), surviving restarts.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use celsus_core::fonts::FontSet;
use celsus_core::pipeline::{Options, Progress, translate_document};
use celsus_core::provider::OpenAiCompatible;
use tokio::runtime::Handle;
use tokio::sync::Notify;

use crate::assist::PgCache;
use crate::{AppState, library, now};

#[derive(Default)]
pub struct Jobs {
    wake: Notify,
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// One writer at a time for each translated PDF (the queue, an assistant in a chat, a redrawn picture).
#[derive(Default)]
pub struct OutputLocks(Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>);

impl OutputLocks {
    pub async fn lock(&self, book: &str, lang: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .0
            .lock()
            .unwrap()
            .entry(format!("{book}/{lang}"))
            .or_default()
            .clone();
        lock.lock_owned().await
    }
}

impl Jobs {
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Ask a running job to stop after its current request. False if it is not running.
    pub fn cancel(&self, id: &str) -> bool {
        match self.running.lock().unwrap().get(id) {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }
}

pub async fn start(state: AppState) -> Result<(), String> {
    let db = state.pool.get().await.map_err(|e| e.to_string())?;
    // Jobs that were running when the server stopped go back in the queue.
    db.execute(
        "UPDATE jobs SET status = 'queued' WHERE status = 'running'",
        &[],
    )
    .await
    .map_err(|e| e.to_string())?;
    tokio::spawn(run_queue(state));
    Ok(())
}

async fn run_queue(state: AppState) {
    loop {
        match next_job(&state).await {
            Ok(Some(id)) => {
                let flag = Arc::new(AtomicBool::new(false));
                state
                    .jobs
                    .running
                    .lock()
                    .unwrap()
                    .insert(id.clone(), flag.clone());
                let outcome = run_job(&state, &id, flag).await;
                state.jobs.running.lock().unwrap().remove(&id);
                let (status, error) = match outcome {
                    Ok(true) => ("cancelled", None),
                    Ok(false) => ("done", None),
                    Err(e) => ("failed", Some(e)),
                };
                if let Ok(db) = state.pool.get().await {
                    let _ = db
                        .execute(
                            "UPDATE jobs SET status = $2, error = $3, finished_at = $4 WHERE id = $1",
                            &[&id, &status, &error, &now()],
                        )
                        .await;
                }
            }
            Ok(None) => {
                let _ =
                    tokio::time::timeout(Duration::from_secs(30), state.jobs.wake.notified()).await;
            }
            Err(e) => {
                tracing::error!("job queue: {e}");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

async fn next_job(state: &AppState) -> Result<Option<String>, String> {
    let db = state.pool.get().await.map_err(|e| e.to_string())?;
    let row = db
        .query_opt(
            "UPDATE jobs SET status = 'running', started_at = $1
             WHERE id = (SELECT id FROM jobs WHERE status = 'queued' ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING id",
            &[&now()],
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|r| r.get(0)))
}

/// The provider and model chosen for a task, with its key decrypted and its address checked.
pub async fn model_for(state: &AppState, task: &str) -> Result<OpenAiCompatible, String> {
    let db = state.pool.get().await.map_err(|e| e.to_string())?;
    let setting: Option<String> = db
        .query_opt(
            "SELECT value FROM settings WHERE key = $1",
            &[&format!("model.{task}")],
        )
        .await
        .map_err(|e| e.to_string())?
        .map(|r| r.get(0));
    let (id, model) = setting
        .as_deref()
        .and_then(|v| v.split_once(':'))
        .and_then(|(id, m)| Some((id.parse::<i64>().ok()?, m.to_owned())))
        .ok_or(match task {
            "image" => "No picture model is chosen. An admin can choose one under Connections and models.",
            "metadata" => "No model is chosen for book details. An admin can choose one under Connections and models.",
            _ => "No model is chosen for translation. An admin can choose one under Connections and models.",
        })?;
    let row = db
        .query_opt(
            "SELECT url, key_sealed, allow_local FROM providers WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| e.to_string())?
        .ok_or("The chosen connection no longer exists.")?;
    let url: String = row.get(0);
    crate::net::check_url(&url, row.get(2)).await?;
    let key = row
        .get::<_, Option<Vec<u8>>>(1)
        .map(|k| {
            state
                .secrets
                .open(&k, &format!("provider:{id}"))
                .ok_or("A connection key could not be decrypted.")
        })
        .transpose()?
        .map(|k| String::from_utf8_lossy(&k).into_owned());
    Ok(OpenAiCompatible::new(url, key, model))
}

async fn db_setting(state: &AppState, key: &str) -> Option<String> {
    let db = state.pool.get().await.ok()?;
    db.query_opt("SELECT value FROM settings WHERE key = $1", &[&key])
        .await
        .ok()?
        .map(|r| r.get(0))
}

/// Run one job. `Ok(true)` means it was cancelled part way; what was done is kept.
async fn run_job(state: &AppState, id: &str, cancel: Arc<AtomicBool>) -> Result<bool, String> {
    let db = state.pool.get().await.map_err(|e| e.to_string())?;
    let job = db
        .query_one(
            "SELECT book_id, lang, pages FROM jobs WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| e.to_string())?;
    let book_id: String = job.get(0);
    let lang: String = job.get(1);
    let wanted: Vec<i32> = job.get(2);
    if !library::valid_id(&book_id) || !library::valid_lang(&lang) {
        return Err("bad job".into());
    }
    let book = db
        .query_one(
            "SELECT title, subject, source_lang FROM books WHERE id = $1",
            &[&book_id],
        )
        .await
        .map_err(|e| e.to_string())?;
    let done: Vec<i32> = db
        .query(
            "SELECT page FROM translated_pages WHERE book_id = $1 AND lang = $2",
            &[&book_id, &lang],
        )
        .await
        .map_err(|e| e.to_string())?
        .iter()
        .map(|r| r.get(0))
        .collect();
    drop(db);
    let client = model_for(state, "translate").await?;
    let model = client.model.clone();

    let mut options = Options::new(
        lang.parse()
            .map_err(|e: celsus_core::language::InvalidLanguage| e.to_string())?,
    );
    options.from = book.get::<_, String>(2).parse().ok();
    if let Some(n) = db_setting(state, "batch_chars")
        .await
        .and_then(|v| v.parse().ok())
    {
        options.batch_chars = n;
    }
    options.book.title = book.get(0);
    options.book.subject = book.get(1);
    let all: BTreeSet<usize> = wanted
        .iter()
        .chain(done.iter())
        .map(|&p| p as usize)
        .collect();
    options.pages = all.into_iter().collect();
    let wanted: BTreeSet<usize> = wanted.iter().map(|&p| p as usize).collect();

    let source = library::source_file(&state.config, &book_id);
    let output = library::output_file(&state.config, &book_id, &lang);
    let fonts_dir = state.config.fonts_dir.clone();
    let target = options.to.clone();
    // Pictures redrawn for this language stay in every new version of the book.
    options.images = library::redrawn_images(&state.config, &book_id, &lang);
    let lock_book = book_id.clone();
    let cache_book = book_id.clone();
    let lock_lang = lang.clone();
    let st = state.clone();
    let handle = Handle::current();
    let job_id = id.to_owned();
    let (bytes, outcome) = tokio::task::spawn_blocking(move || {
        let fonts = FontSet::for_language(&fonts_dir, &target);
        let doc = celsus_pdf::Document::open(&source).map_err(|e| e.to_string())?;
        let mut cache = PgCache {
            state: st.clone(),
            handle: handle.clone(),
            book_id: cache_book,
        };
        let mut done = 0i32;
        let mut progress = |p: Progress| {
            if let Progress::PageDone { page, kept, .. } = p {
                if wanted.contains(&page) {
                    done += 1;
                }
                handle.block_on(async {
                    if let Ok(db) = st.pool.get().await {
                        let _ = db
                            .execute(
                                "INSERT INTO translated_pages (book_id, lang, page, kept) VALUES ($1, $2, $3, $4)
                                 ON CONFLICT (book_id, lang, page) DO UPDATE SET kept = $4",
                                &[&book_id, &lang, &(page as i32), &(kept as i32)],
                            )
                            .await;
                        let _ = db.execute("UPDATE jobs SET done_pages = $2 WHERE id = $1", &[&job_id, &done]).await;
                    }
                });
            }
        };
        translate_document(&doc, &fonts, &client, &options, &mut cache, &mut progress, &cancel).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    // Write next to the old file and swap, so readers never see half a PDF.
    let _guard = state.outputs.lock(&lock_book, &lock_lang).await;
    let tmp = output.with_extension("pdf.part");
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::rename(&tmp, &output)
        .await
        .map_err(|e| e.to_string())?;
    let db = state.pool.get().await.map_err(|e| e.to_string())?;
    db.execute(
        "UPDATE jobs SET prompt_tokens = $2, completion_tokens = $3, model = $4 WHERE id = $1",
        &[
            &id,
            &(outcome.usage.prompt_tokens as i64),
            &(outcome.usage.completion_tokens as i64),
            &model,
        ],
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(outcome.cancelled)
}
