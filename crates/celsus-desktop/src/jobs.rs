//! The translation queue: one job at a time, in the background, surviving restarts.
//!
//! Each job translates its pages and then rewrites the book's output for that language with every page translated
//! so far. Pages done earlier come from the translation cache, so they cost nothing again.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use celsus_core::fonts::FontSet;
use celsus_core::pipeline::{Options, Progress, translate_document};
use celsus_core::provider::OpenAiCompatible;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use celsus_store::{db, library, secrets};

/// The running job: its id, its book and its cancel flag.
type Current = Arc<Mutex<Option<(i64, i64, Arc<AtomicBool>)>>>;

#[derive(Clone)]
pub struct Runner {
    wake: Sender<()>,
    current: Current,
}

impl Runner {
    pub fn wake(&self) {
        let _ = self.wake.send(());
    }

    /// Stop the job if it is the one running; returns false if it is not.
    pub fn cancel_running(&self, job: i64) -> bool {
        match &*self.current.lock().unwrap() {
            Some((id, _, flag)) if *id == job => {
                flag.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    /// Stop whatever is running on a book, so a book that is being removed is not translated (and paid for)
    /// any further.
    pub fn cancel_book(&self, book: i64) {
        if let Some((_, b, flag)) = &*self.current.lock().unwrap()
            && *b == book
        {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Serialize)]
struct JobProgress {
    job_id: i64,
    book_id: i64,
    page: usize,
    done: usize,
    total: usize,
    message: Option<String>,
}

pub fn start(app: AppHandle, db_path: PathBuf, library: PathBuf) -> Runner {
    let (wake, rx) = channel();
    let current = Arc::new(Mutex::new(None));
    let runner = Runner {
        wake,
        current: current.clone(),
    };
    std::thread::spawn(move || worker(app, &db_path, &library, rx, current));
    runner
}

fn worker(app: AppHandle, db_path: &Path, library: &Path, wake: Receiver<()>, current: Current) {
    let conn = match db::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("job queue: cannot open database: {e}");
            return;
        }
    };
    let _ = db::requeue_interrupted(&conn);
    loop {
        match db::next_queued(&conn) {
            Ok(Some(id)) => {
                let cancel = Arc::new(AtomicBool::new(false));
                let book = db::job(&conn, id).map(|j| j.book_id).unwrap_or(-1);
                *current.lock().unwrap() = Some((id, book, cancel.clone()));
                let outcome = run(&app, &conn, library, id, &cancel);
                *current.lock().unwrap() = None;
                let (status, error) = match outcome {
                    Ok(true) => ("cancelled", None),
                    Ok(false) => ("done", None),
                    Err(e) => ("failed", Some(e)),
                };
                let _ = db::set_status(&conn, id, status, error.as_deref());
                if let Ok(job) = db::job(&conn, id) {
                    let _ = app.emit("job-updated", job);
                }
                let _ = app.emit("library-changed", ());
            }
            _ => {
                // Wait for a new job (or check again now and then).
                let _ = wake.recv_timeout(Duration::from_secs(30));
            }
        }
    }
}

/// Run one job. `Ok(true)` means it was cancelled part way (what was done is kept).
fn run(
    app: &AppHandle,
    conn: &rusqlite::Connection,
    library: &Path,
    id: i64,
    cancel: &Arc<AtomicBool>,
) -> Result<bool, String> {
    let job = db::job(conn, id)?;
    let book = db::book(conn, job.book_id)?;
    db::set_status(conn, id, "running", None)?;
    let _ = app.emit("job-updated", db::job(conn, id)?);

    let (provider, model) = db::model_for(conn, "translate")?.ok_or("no_model")?;
    let client = OpenAiCompatible::new(
        provider.url.clone(),
        secrets::get_key(provider.id),
        model.clone(),
    )
    .stopped_by(cancel.clone())
    .answers_within(
        db::setting(conn, "answer_wait")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(celsus_core::provider::ANSWER_WAIT),
    );

    let fonts_dir = db::setting(conn, "fonts_dir")?
        .filter(|d| !d.is_empty())
        .unwrap_or_else(crate::default_fonts_dir);
    let doc = celsus_pdf::Document::open(library::source_file(library, book.id))
        .map_err(|e| e.to_string())?;
    let mut options = Options::new(
        job.target_lang
            .parse()
            .map_err(|e: celsus_core::language::InvalidLanguage| e.to_string())?,
    );
    let fonts = FontSet::for_language(Path::new(&fonts_dir), &options.to);
    options.from = book.source_lang.parse().ok();
    options.book.title = book.title.clone();
    options.book.subject = book.subject.clone();
    options.images = library::redrawn_images(library, book.id, &job.target_lang);
    if let Some(n) = db::setting(conn, "batch_chars")?.and_then(|v| v.parse().ok()) {
        options.batch_chars = n;
    }
    // This job's pages, plus every page translated before, so the output holds all of them.
    let wanted: BTreeSet<i64> = job.pages.iter().copied().collect();
    let translated: BTreeSet<i64> = db::translated_pages(conn, book.id, &job.target_lang)?
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    let all: BTreeSet<i64> = wanted.union(&translated).copied().collect();
    options.pages = all.iter().map(|&p| p as usize).collect();
    // What is left to do. A job that was stopped goes on from where it was: its pages that are translated
    // already count as done from the start.
    let todo: BTreeSet<i64> = wanted.difference(&translated).copied().collect();
    // Pages translated before are only written again: nothing is asked, and so nothing is paid, for them.
    options.settled = all.difference(&todo).map(|&p| p as usize).collect();

    let total = wanted.len();
    let mut done = wanted.len() - todo.len();
    // Pages translated by this run.
    let mut fresh = 0usize;
    db::job_counters(conn, id, done as i64, total as i64, 0, 0, 0, 0, 0, &model)?;
    let _ = app.emit("job-updated", db::job(conn, id)?);
    // Earlier pages whose translation could not be written again.
    let mut lost = 0usize;
    // Blocks of text the pages of this job had to translate.
    let mut texts = 0usize;
    // Pages of this job where the model gave no usable answer for some of the text.
    let mut unfinished = 0usize;
    let (mut replaced, mut kept) = (0i64, 0i64);
    let mut progress = |p: Progress| {
        let mut message = None;
        let mut page = 0;
        match p {
            Progress::PageDone {
                page: n,
                translated,
                replaced: r,
                kept: k,
                missing,
            } => {
                page = n;
                let n64 = n as i64;
                if todo.contains(&n64) {
                    texts += translated + missing;
                }
                // A page with text left untranslated is not marked, so "missing pages" picks it up again.
                if missing == 0 {
                    let _ = db::mark_page(conn, book.id, &job.target_lang, n64, k as i64);
                } else if !todo.contains(&n64) {
                    // It stays as the original in the new file, and is not called translated any more.
                    let _ = db::unmark_page(conn, book.id, &job.target_lang, n64);
                    lost += 1;
                    if wanted.contains(&n64) {
                        done = done.saturating_sub(1);
                    }
                }
                if todo.contains(&n64) {
                    if missing == 0 {
                        done += 1;
                        fresh += 1;
                    } else {
                        unfinished += 1;
                    }
                    replaced += r as i64;
                    kept += k as i64;
                }
                let _ = db::job_counters(
                    conn,
                    id,
                    done as i64,
                    total as i64,
                    replaced,
                    kept,
                    0,
                    0,
                    0,
                    &model,
                );
            }
            Progress::PageStarted { page: n, .. } => page = n,
            Progress::Waiting { seconds } => message = Some(format!("wait:{seconds}")),
            Progress::Note { message: m } => message = Some(m),
            Progress::Started { .. } => {}
        }
        let _ = app.emit(
            "job-progress",
            JobProgress {
                job_id: id,
                book_id: book.id,
                page,
                done,
                total,
                message,
            },
        );
    };
    let mut cache = db::DbCache(conn);
    let (bytes, outcome) = translate_document(
        &doc,
        &fonts,
        &client,
        &options,
        &mut cache,
        &mut progress,
        cancel,
    )
    .map_err(|e| e.to_string())?;
    // The counters first: what a failed run used was still spent.
    db::job_counters(
        conn,
        id,
        done as i64,
        total as i64,
        replaced,
        kept,
        outcome.requests as i64,
        outcome.usage.prompt_tokens as i64,
        outcome.usage.completion_tokens as i64,
        &model,
    )?;
    // A run that stopped early still has its finished pages; only one that wrote nothing leaves the file alone.
    if outcome.pages > 0 {
        let output = library::output_file(library, book.id, &job.target_lang);
        // Written beside the old file and swapped in, so a crash cannot leave half a PDF.
        let part = output.with_extension("pdf.part");
        std::fs::write(&part, &bytes).map_err(|e| e.to_string())?;
        if std::fs::rename(&part, &output).is_err() {
            // The old file is open somewhere; write over it instead.
            std::fs::write(&output, &bytes).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&part);
        }
    }
    // The interface has words of its own for a model that stayed silent.
    if outcome.no_answer {
        return Err("no_answer".into());
    }
    if let Some(why) = outcome.failed {
        return Err(why);
    }
    if unfinished > 0 && !outcome.cancelled {
        return Err(format!(
            "The model gave no usable answer for some text on {unfinished} page(s). Translate the missing pages again."
        ));
    }
    // Pages that are only pictures (a scanned book) come through with nothing done to them. That is not a
    // translation: they are not called translated, and the job says why.
    if texts == 0 && fresh > 0 && !outcome.cancelled {
        for &n in &todo {
            let _ = db::unmark_page(conn, book.id, &job.target_lang, n);
        }
        return Err("no_text".into());
    }
    if lost > 0 && !outcome.cancelled {
        return Err(format!(
            "The translation of {lost} earlier page(s) was no longer kept, so they are back to the original. Translate the missing pages again."
        ));
    }
    Ok(outcome.cancelled)
}
