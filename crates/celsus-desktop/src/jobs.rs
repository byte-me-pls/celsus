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
    // The queue of a library is worked by one app at a time. Another copy left open (an older one, or one
    // installed somewhere else) holds it, and this one waits until that one is closed.
    let _lock = queue_lock(db_path);
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
            // Taken from the queue only if nobody took it in the meantime.
            Ok(Some(id)) if !db::claim_job(&conn, id).unwrap_or(false) => continue,
            Ok(Some(id)) => {
                let cancel = Arc::new(AtomicBool::new(false));
                let book = db::job(&conn, id).map(|j| j.book_id).unwrap_or(-1);
                *current.lock().unwrap() = Some((id, book, cancel.clone()));
                let before = translated(&conn, id);
                let outcome = run(&app, &conn, library, id, &cancel);
                *current.lock().unwrap() = None;
                let fresh = translated(&conn, id) > before;
                match outcome {
                    Ok(cancelled) => {
                        let status = if cancelled { "cancelled" } else { "done" };
                        let _ = db::set_status(&conn, id, status, None);
                    }
                    // A model that stayed silent, a connection that dropped, a limit that was reached: these pass.
                    // The job is tried again on its own after a while, as long as the tries get it further.
                    Err(e) => match retry_after(&e, db::retries(&conn, id).unwrap_or(0), fresh) {
                        Some(wait) => {
                            let at = db::now() + wait;
                            let _ = db::schedule_retry(&conn, id, &e, at, fresh);
                        }
                        None => {
                            let _ = db::set_status(&conn, id, "failed", Some(&e));
                        }
                    },
                }
                if let Ok(job) = db::job(&conn, id) {
                    // Stopped and failed jobs of the book that have nothing left to go on with leave the list.
                    if let Ok(n) = db::hide_superseded(&conn, job.book_id, &job.target_lang)
                        && n > 0
                    {
                        let _ = app.emit("jobs-changed", ());
                    }
                    let _ = app.emit("job-updated", job);
                }
                let _ = app.emit("library-changed", ());
            }
            _ => {
                // Wait for a new job, or for a job's time to be tried again (looked for now and then).
                let _ = wake.recv_timeout(Duration::from_secs(30));
            }
        }
    }
}

/// The lock on the queue of the library beside `db_path`, held for as long as the returned file is open: the
/// system lets it go when the app ends, however it ends.
fn queue_lock(db_path: &Path) -> std::fs::File {
    let path = db_path.with_file_name("jobs.lock");
    loop {
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            && file.try_lock().is_ok()
        {
            return file;
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// How many pages of the job's book are translated into its language.
fn translated(conn: &rusqlite::Connection, id: i64) -> usize {
    db::job(conn, id)
        .and_then(|job| db::translated_pages(conn, job.book_id, &job.target_lang))
        .map_or(0, |pages| pages.len())
}

/// How long to wait before a failed job is tried again, if it is to be: when it failed for a reason that
/// passes, and it has not been tried this many times already without getting further. `tries` is that count so
/// far, and `fresh` says the run that failed got some pages done, which starts the count again.
fn retry_after(error: &str, tries: i64, fresh: bool) -> Option<i64> {
    const WAITS: [i64; 3] = [2 * 60, 5 * 60, 15 * 60];
    let passing = error == "no_answer"
        || error.contains("provider error")
        || error.contains("kept refusing requests");
    let tries = if fresh { 0 } else { tries };
    passing
        .then(|| WAITS.get(tries as usize).copied())
        .flatten()
}

/// Write the translated book. It is written beside the old file and swapped in, so a crash cannot leave half
/// a PDF.
fn write_output(output: &Path, bytes: &[u8]) -> Result<(), String> {
    let part = output.with_extension("pdf.part");
    std::fs::write(&part, bytes).map_err(|e| e.to_string())?;
    if std::fs::rename(&part, output).is_err() {
        // The old file is open somewhere; write over it instead.
        std::fs::write(output, bytes).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&part);
    }
    Ok(())
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
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| {
                v.clamp(
                    *celsus_core::provider::ANSWER_WAIT_RANGE.start(),
                    *celsus_core::provider::ANSWER_WAIT_RANGE.end(),
                )
            })
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
    if let Some(n) = db::setting(conn, "batch_chars")?.and_then(|v| v.parse::<usize>().ok()) {
        options.batch_chars = n.clamp(
            *celsus_core::pipeline::BATCH_CHARS.start(),
            *celsus_core::pipeline::BATCH_CHARS.end(),
        );
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
    // The file is written every few seconds while the job runs, so that a page shown as translated is
    // translated in the reader too, long before the job ends.
    options.written_every = 5;
    let output = library::output_file(library, book.id, &job.target_lang);

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
            Progress::Written { bytes } => {
                if write_output(&output, &bytes).is_ok() {
                    let _ = app.emit("book-written", (book.id, &job.target_lang));
                }
                return;
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
        write_output(&output, &bytes)?;
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

#[cfg(test)]
mod tests {
    use super::retry_after;

    #[test]
    fn what_passes_is_tried_again_as_long_as_it_gets_further() {
        assert_eq!(retry_after("no_answer", 0, false), Some(120));
        assert_eq!(
            retry_after("page 3: provider error: timed out", 1, false),
            Some(300)
        );
        assert_eq!(retry_after("no_answer", 2, false), Some(900));
        assert_eq!(retry_after("no_answer", 3, false), None);
        // A run that translated pages before it failed starts the count again.
        assert_eq!(retry_after("no_answer", 3, true), Some(120));
        // What will not pass by waiting is not tried again.
        assert_eq!(
            retry_after("page 1: provider refused: HTTP 401", 0, true),
            None
        );
        assert_eq!(retry_after("no_model", 0, false), None);
    }
}
