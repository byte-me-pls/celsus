// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod jobs;
mod mcp_clients;

use celsus_store::{db, library, secrets};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use celsus_core::metadata::{BookInfo, BookSample, identify};
use celsus_core::provider::OpenAiCompatible;
use celsus_core::translate::{BookContext, TranslationRequest, Translator};
use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

struct App {
    db: Mutex<Connection>,
    library: PathBuf,
    db_path: PathBuf,
    runner: jobs::Runner,
}

type Res<T> = Result<T, String>;

pub use celsus_store::default_fonts_dir;

/// Values from a `.env` file in the working folder or one of its parents (used to set up a first provider while
/// developing).
fn dotenv() -> BTreeMap<String, String> {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        if let Ok(text) = std::fs::read_to_string(d.join(".env")) {
            return text
                .lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().trim_matches('"').to_owned()))
                .collect();
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    BTreeMap::new()
}

fn seed_provider(conn: &Connection) -> Res<()> {
    if !db::providers(conn)?.is_empty() {
        return Ok(());
    }
    let env = dotenv();
    let get = |k: &str| {
        std::env::var(k)
            .ok()
            .or_else(|| env.get(k).cloned())
            .filter(|v| !v.is_empty())
    };
    let (Some(url), Some(model)) = (get("CELSUS_API_URL"), get("CELSUS_MODEL")) else {
        return Ok(());
    };
    let name = if url.contains("nvidia") {
        "NVIDIA"
    } else if url.contains("openai") {
        "OpenAI"
    } else {
        "Model"
    };
    let id = db::save_provider(
        conn,
        &db::Provider {
            id: 0,
            name: name.into(),
            url,
            model,
            is_default: true,
            has_key: false,
        },
    )?;
    if let Some(key) = get("CELSUS_API_KEY")
        .or_else(|| get("NVIDIA_API_KEY"))
        .or_else(|| get("OPENAI_API_KEY"))
    {
        secrets::set_key(id, &key)?;
    }
    Ok(())
}

// ---------- library ----------

#[tauri::command]
fn library_list(app: State<App>) -> Res<Vec<db::Book>> {
    db::books(&app.db.lock().unwrap())
}

#[tauri::command]
fn book_get(app: State<App>, id: i64) -> Res<db::Book> {
    db::book(&app.db.lock().unwrap(), id)
}

#[tauri::command]
async fn pick_pdfs(handle: AppHandle) -> Vec<String> {
    handle
        .dialog()
        .file()
        .add_filter("PDF", &["pdf"])
        .blocking_pick_files()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| f.into_path().ok())
        .map(|p| p.display().to_string())
        .collect()
}

#[derive(Serialize)]
struct AddResult {
    added: Vec<db::Book>,
    failed: Vec<String>,
}

#[tauri::command]
async fn books_add(
    handle: AppHandle,
    paths: Vec<String>,
    ui_lang: Option<String>,
) -> Res<AddResult> {
    let app = handle.state::<App>();
    let mut result = AddResult {
        added: Vec::new(),
        failed: Vec::new(),
    };
    for p in paths {
        let conn = app.db.lock().unwrap();
        match library::add_book(&conn, &app.library, Path::new(&p)) {
            Ok(b) => result.added.push(b),
            Err(e) => result.failed.push(e),
        }
    }
    let _ = handle.emit("library-changed", ());
    // Fill in what the file does not say (author, subject, language) in the background.
    let ids: Vec<i64> = result.added.iter().map(|b| b.id).collect();
    let lang = ui_lang.unwrap_or_else(|| "en".into());
    let h = handle.clone();
    std::thread::spawn(move || {
        for id in ids {
            if let Err(e) = recognise_and_fill(&h, id, &lang) {
                eprintln!("could not recognise book {id}: {e}");
            }
        }
    });
    Ok(result)
}

/// A client for the model chosen for `task`.
fn client_for(app: &App, task: &str) -> Res<OpenAiCompatible> {
    let (provider, model) = db::model_for(&app.db.lock().unwrap(), task)?.ok_or_else(|| {
        match task {
            "image" => "Choose a picture model in Settings → Models first.",
            _ => "Choose a model in Settings → Models first.",
        }
        .to_owned()
    })?;
    Ok(OpenAiCompatible::new(
        provider.url,
        secrets::get_key(provider.id),
        model,
    ))
}

/// Ask the book model what this book is.
fn recognise(app: &App, id: i64, ui_lang: &str) -> Res<BookInfo> {
    let book = db::book(&app.db.lock().unwrap(), id)?;
    let client = client_for(app, "metadata")?;
    let doc = celsus_pdf::Document::open(library::source_file(&app.library, id))
        .map_err(|e| e.to_string())?;
    let sample = BookSample {
        file_name: book.original_name.clone(),
        title: book.title.clone(),
        author: book.author.clone(),
        text: library::opening_text(&doc, 3500),
        answer_language: ui_lang.to_owned(),
    };
    identify(&client, &sample)
        .map(|(info, _)| info)
        .map_err(|e| e.to_string())
}

/// Recognise a newly added book and fill the details that are still empty. A title taken from the file name is
/// replaced; one from the document's own metadata is kept.
fn recognise_and_fill(handle: &AppHandle, id: i64, ui_lang: &str) -> Res<()> {
    let app = handle.state::<App>();
    if db::model_for(&app.db.lock().unwrap(), "metadata")?.is_none() {
        return Ok(());
    }
    let info = recognise(&app, id, ui_lang)?;
    {
        let conn = app.db.lock().unwrap();
        let book = db::book(&conn, id)?;
        let from_file_name = book.title
            == book
                .original_name
                .trim_end_matches(".pdf")
                .trim_end_matches(".PDF")
                .replace(['_', '-'], " ");
        let pick = |current: &str, found: &str, replace: bool| {
            if (current.trim().is_empty() || replace) && !found.is_empty() {
                found.to_owned()
            } else {
                current.to_owned()
            }
        };
        db::update_book(
            &conn,
            id,
            &pick(&book.title, &info.title, from_file_name && info.recognised),
            &pick(&book.author, &info.author, false),
            &pick(&book.subject, &info.subject, false),
            &pick(&book.source_lang, &info.language, false),
        )?;
    }
    let _ = handle.emit("library-changed", ());
    let _ = handle.emit("book-recognised", id);
    Ok(())
}

/// What the book model says this book is, without saving it.
#[tauri::command]
async fn book_recognise(handle: AppHandle, id: i64, ui_lang: String) -> Res<BookInfo> {
    recognise(&handle.state::<App>(), id, &ui_lang)
}

#[tauri::command]
fn book_update(
    app: State<App>,
    id: i64,
    title: String,
    author: String,
    subject: String,
    source_lang: String,
) -> Res<db::Book> {
    let conn = app.db.lock().unwrap();
    db::update_book(
        &conn,
        id,
        title.trim(),
        author.trim(),
        subject.trim(),
        source_lang.trim(),
    )?;
    db::book(&conn, id)
}

#[tauri::command]
fn book_delete(handle: AppHandle, app: State<App>, id: i64) -> Res<()> {
    app.runner.cancel_book(id);
    library::remove_book(&app.db.lock().unwrap(), &app.library, id)?;
    let _ = handle.emit("library-changed", ());
    Ok(())
}

#[derive(Serialize)]
struct BookFiles {
    source: String,
    cover: Option<String>,
    outputs: BTreeMap<String, String>,
}

/// Where a book's files are. `open` says the reader is opening it, which moves the book to the front of the
/// library; looking the files up for a cover does not.
#[tauri::command]
fn book_files(app: State<App>, id: i64, open: Option<bool>) -> Res<BookFiles> {
    let conn = app.db.lock().unwrap();
    let book = db::book(&conn, id)?;
    if open == Some(true) {
        db::touch_book(&conn, id)?;
    }
    let outputs = book
        .translated
        .keys()
        .map(|lang| (lang.clone(), library::output_file(&app.library, id, lang)))
        .filter(|(_, p)| p.exists())
        .map(|(l, p)| (l, p.display().to_string()))
        .collect();
    Ok(BookFiles {
        source: library::source_file(&app.library, id).display().to_string(),
        cover: book
            .has_cover
            .then(|| library::cover_file(&app.library, id).display().to_string()),
        outputs,
    })
}

/// Keep the cover the page drew for a book: its first page as a small JPEG.
#[tauri::command]
fn cover_save(app: State<App>, id: i64, jpeg: Vec<u8>) -> Res<()> {
    library::set_cover(&app.db.lock().unwrap(), &app.library, id, &jpeg)
}

#[tauri::command]
fn book_pages(app: State<App>, id: i64, lang: String) -> Res<Vec<(i64, i64)>> {
    db::translated_pages(&app.db.lock().unwrap(), id, &lang)
}

/// Characters of text on the given pages, for the cost estimate.
#[tauri::command]
async fn book_estimate(handle: AppHandle, id: i64, pages: Vec<i64>) -> Res<u64> {
    let app = handle.state::<App>();
    let path = library::source_file(&app.library, id);
    let doc = celsus_pdf::Document::open(&path).map_err(|e| e.to_string())?;
    let all = doc.pages().map_err(|e| e.to_string())?;
    let mut chars = 0u64;
    for n in pages {
        let Some(page) = all.get(n as usize - 1) else {
            continue;
        };
        if let Ok(e) = celsus_core::extract::extract_page(&doc, page, n as u32) {
            chars += e
                .layout
                .blocks
                .iter()
                .filter(|b| !b.protected)
                .map(|b| b.plain().chars().count() as u64)
                .sum::<u64>();
        }
    }
    Ok(chars)
}

#[tauri::command]
async fn save_output(handle: AppHandle, id: i64, lang: String) -> Res<Option<String>> {
    let app = handle.state::<App>();
    // The language names a file: only a real language tag will do.
    parse_lang(&lang)?;
    let source = library::output_file(&app.library, id, &lang);
    let title = db::book(&app.db.lock().unwrap(), id)?.title;
    let suggested: String = title
        .chars()
        .map(|c| if r#"\/:*?"<>|"#.contains(c) { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned();
    let Some(target) = handle
        .dialog()
        .file()
        .add_filter("PDF", &["pdf"])
        .set_file_name(format!("{suggested} ({lang}).pdf"))
        .blocking_save_file()
        .and_then(|f| f.into_path().ok())
    else {
        return Ok(None);
    };
    std::fs::copy(&source, &target).map_err(|e| e.to_string())?;
    Ok(Some(target.display().to_string()))
}

/// A file's bytes, sent to the page as raw binary (PDF previews, covers).
#[tauri::command]
async fn read_file(path: String) -> Res<tauri::ipc::Response> {
    std::fs::read(&path)
        .map(tauri::ipc::Response::new)
        .map_err(|e| e.to_string())
}

// ---------- pictures ----------

#[derive(Serialize)]
struct ImageInfo {
    image: String,
    width: u32,
    height: u32,
    /// The redrawn file in the chosen language, if there is one.
    redrawn: Option<String>,
}

#[tauri::command]
async fn page_images(handle: AppHandle, id: i64, page: usize, lang: String) -> Res<Vec<ImageInfo>> {
    let app = handle.state::<App>();
    let doc = celsus_pdf::Document::open(library::source_file(&app.library, id))
        .map_err(|e| e.to_string())?;
    let pages = doc.pages().map_err(|e| e.to_string())?;
    let p = pages.get(page.wrapping_sub(1)).ok_or("No such page.")?;
    Ok(celsus_pdf::image::page_images(&doc, p)
        .into_iter()
        .map(|i| {
            let file = library::find_redrawn(&app.library, id, &lang, page, i.id);
            ImageInfo {
                image: format!("{} {}", i.id.num, i.id.generation),
                width: i.width,
                height: i.height,
                redrawn: file.map(|f| f.display().to_string()),
            }
        })
        .collect())
}

/// The original picture as an image file (JPEG or PNG).
#[tauri::command]
async fn image_original(handle: AppHandle, id: i64, image: String) -> Res<tauri::ipc::Response> {
    let app = handle.state::<App>();
    let doc = celsus_pdf::Document::open(library::source_file(&app.library, id))
        .map_err(|e| e.to_string())?;
    let obj = library::parse_image_id(&image).ok_or("bad picture id")?;
    let (bytes, _) =
        celsus_pdf::image::export(&doc, obj).ok_or("This picture's format cannot be read.")?;
    Ok(tauri::ipc::Response::new(bytes))
}

fn parse_lang(lang: &str) -> Res<celsus_core::Language> {
    lang.parse()
        .map_err(|e: celsus_core::language::InvalidLanguage| e.to_string())
}

/// Redraw a picture with its text in `lang`, then rewrite the translated book with it.
#[tauri::command]
async fn image_redraw(
    handle: AppHandle,
    id: i64,
    lang: String,
    page: usize,
    image: String,
    note: Option<String>,
) -> Res<String> {
    let app = handle.state::<App>();
    let to = parse_lang(&lang)?;
    let client = celsus_store::pictures::client(&app.db.lock().unwrap())?;
    // The model call takes a while; the database is locked only to write the book afterwards.
    let book = db::book(&app.db.lock().unwrap(), id)?;
    let doc = celsus_pdf::Document::open(library::source_file(&app.library, id))
        .map_err(|e| e.to_string())?;
    let obj = library::parse_image_id(&image).ok_or("bad picture id")?;
    let pages = doc.pages().map_err(|e| e.to_string())?;
    let p = pages.get(page.wrapping_sub(1)).ok_or("No such page.")?;
    let info = celsus_pdf::image::page_images(&doc, p)
        .into_iter()
        .find(|i| i.id == obj)
        .ok_or("The picture is not on this page.")?;
    let (bytes, mime) =
        celsus_pdf::image::export(&doc, obj).ok_or("This picture's format cannot be read.")?;
    let aspect = info.width as f32 / info.height.max(1) as f32;
    let instructions = celsus_store::pictures::instructions(&book, page, &to, note.as_deref());
    let picture = client
        .edit_image(&bytes, mime, aspect, &instructions)
        .map_err(|e| e.to_string())?;
    let file = celsus_store::pictures::place(
        &app.db.lock().unwrap(),
        &app.library,
        id,
        &to,
        page,
        Some(&image),
        &picture,
    )?;
    let _ = handle.emit("library-changed", ());
    Ok(file.display().to_string())
}

/// Go back to the original picture.
#[tauri::command]
async fn image_restore(
    handle: AppHandle,
    id: i64,
    lang: String,
    page: usize,
    image: String,
) -> Res<()> {
    let app = handle.state::<App>();
    let to = parse_lang(&lang)?;
    celsus_store::pictures::restore(
        &app.db.lock().unwrap(),
        &app.library,
        id,
        &to,
        page,
        Some(&image),
    )?;
    let _ = handle.emit("library-changed", ());
    Ok(())
}

// ---------- jobs ----------

#[tauri::command]
fn job_create(
    handle: AppHandle,
    app: State<App>,
    book_id: i64,
    lang: String,
    pages: Vec<i64>,
) -> Res<db::Job> {
    lang.parse::<celsus_core::Language>()
        .map_err(|e| e.to_string())?;
    // One spelling per language: "TR" and "tr" are the same translation, and the same file.
    let lang = lang.trim().to_lowercase();
    let mut pages = pages;
    pages.sort_unstable();
    pages.dedup();
    if pages.is_empty() {
        return Err("Choose at least one page.".into());
    }
    let job = {
        let conn = app.db.lock().unwrap();
        if db::model_for(&conn, "translate")?.is_none() {
            return Err("no_model".into());
        }
        // The same request sent twice (a double click) is one job.
        let same = db::jobs(&conn, Some(book_id))?.into_iter().find(|j| {
            matches!(j.status.as_str(), "queued" | "running")
                && j.target_lang == lang
                && j.pages == pages
        });
        match same {
            Some(j) => return Ok(j),
            None => {
                let id = db::insert_job(&conn, book_id, &lang, &pages)?;
                db::job(&conn, id)?
            }
        }
    };
    app.runner.wake();
    let _ = handle.emit("job-updated", job.clone());
    let _ = handle.emit("library-changed", ());
    Ok(job)
}

#[tauri::command]
fn jobs_list(app: State<App>, book_id: Option<i64>) -> Res<Vec<db::Job>> {
    db::jobs(&app.db.lock().unwrap(), book_id)
}

#[tauri::command]
fn job_cancel(handle: AppHandle, app: State<App>, id: i64) -> Res<()> {
    if !app.runner.cancel_running(id) {
        let conn = app.db.lock().unwrap();
        db::set_status(&conn, id, "cancelled", None)?;
        let _ = handle.emit("job-updated", db::job(&conn, id)?);
        let _ = handle.emit("library-changed", ());
    }
    Ok(())
}

#[tauri::command]
fn job_retry(handle: AppHandle, app: State<App>, id: i64) -> Res<()> {
    {
        let conn = app.db.lock().unwrap();
        db::set_status(&conn, id, "queued", None)?;
        let _ = handle.emit("job-updated", db::job(&conn, id)?);
    }
    app.runner.wake();
    Ok(())
}

// ---------- providers ----------

#[tauri::command]
fn providers_list(app: State<App>) -> Res<Vec<db::Provider>> {
    let mut list = db::providers(&app.db.lock().unwrap())?;
    for p in &mut list {
        p.has_key = secrets::get_key(p.id).is_some();
    }
    Ok(list)
}

/// Save a provider. `key`: `None` keeps the stored key, an empty string removes it.
#[tauri::command]
fn provider_save(app: State<App>, provider: db::Provider, key: Option<String>) -> Res<i64> {
    if !web_address(&provider.url) {
        return Err("bad_url".into());
    }
    let id = db::save_provider(&app.db.lock().unwrap(), &provider)?;
    if let Some(k) = key {
        secrets::set_key(id, k.trim())?;
    }
    Ok(id)
}

#[tauri::command]
fn provider_delete(app: State<App>, id: i64) -> Res<()> {
    db::delete_provider(&app.db.lock().unwrap(), id)?;
    secrets::delete_key(id);
    Ok(())
}

#[tauri::command]
fn provider_default(app: State<App>, id: i64) -> Res<()> {
    db::set_default_provider(&app.db.lock().unwrap(), id)
}

#[derive(Serialize)]
struct TestResult {
    ok: bool,
    millis: u128,
    sample: String,
    error: Option<String>,
    /// The connection asked to wait this many seconds before the next request.
    wait: Option<u64>,
}

/// Ask the provider to translate one short sentence.
#[tauri::command]
async fn provider_test(
    url: String,
    model: String,
    key: Option<String>,
    id: Option<i64>,
) -> TestResult {
    let key = key
        .filter(|k| !k.is_empty())
        .or_else(|| id.and_then(secrets::get_key));
    translate_test(OpenAiCompatible::new(url, key, model))
}

fn translate_test(client: OpenAiCompatible) -> TestResult {
    let request = TranslationRequest {
        source_language: "en".parse().ok(),
        target_language: "tr".parse().unwrap(),
        book: BookContext::default(),
        previous_text: String::new(),
        next_text: String::new(),
        blocks: vec![(
            celsus_core::BlockId("b1".into()),
            "A library is a garden of books.".into(),
        )],
        feedback: Vec::new(),
    };
    let start = std::time::Instant::now();
    match client.translate(&request) {
        Ok((t, _)) => TestResult {
            ok: true,
            millis: start.elapsed().as_millis(),
            sample: t.join(" "),
            error: None,
            wait: None,
        },
        Err(e) => TestResult {
            ok: false,
            millis: start.elapsed().as_millis(),
            sample: String::new(),
            wait: match &e {
                celsus_core::translate::TranslateError::RateLimited { retry_after_secs } => {
                    Some(*retry_after_secs)
                }
                _ => None,
            },
            error: Some(e.to_string()),
        },
    }
}

/// Whether `url` can be the address of a model service: http or https, with a host after it.
fn web_address(url: &str) -> bool {
    ["https://", "http://"].iter().any(|scheme| {
        url.trim()
            .strip_prefix(scheme)
            .is_some_and(|rest| !rest.trim_matches('/').is_empty())
    })
}

/// Try a connection before it is relied on: its address, whether it answers, and whether it takes the key.
/// Gives the number of models it lists.
#[tauri::command]
async fn provider_check(url: String, key: Option<String>, id: Option<i64>) -> Res<usize> {
    if !web_address(&url) {
        return Err("bad_url".into());
    }
    let key = key
        .filter(|k| !k.is_empty())
        .or_else(|| id.and_then(secrets::get_key));
    let client = OpenAiCompatible::new(url, key, String::new());
    // Some services list their models to anyone, so the list alone says nothing about the key.
    let refused = |e: &celsus_core::translate::TranslateError| matches!(e, celsus_core::translate::TranslateError::Fatal(why) if why.starts_with("HTTP 401") || why.starts_with("HTTP 403"));
    let models = match client.list_models() {
        Ok(models) => models.len(),
        Err(e) if refused(&e) => return Err("bad_key".into()),
        Err(e) => return Err(e.to_string()),
    };
    match client.check_key() {
        Ok(()) => Ok(models),
        Err(e) if refused(&e) => Err("bad_key".into()),
        // Anything else is about that one request, not about the key.
        Err(_) => Ok(models),
    }
}

#[tauri::command]
async fn provider_models(url: String, key: Option<String>, id: Option<i64>) -> Res<Vec<String>> {
    let key = key
        .filter(|k| !k.is_empty())
        .or_else(|| id.and_then(secrets::get_key));
    OpenAiCompatible::new(url, key, String::new())
        .list_models()
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct TaskModel {
    task: String,
    provider_id: Option<i64>,
    model: String,
    /// Chosen explicitly (rather than the fallback).
    chosen: bool,
}

/// Which connection and model does each job: translating, recognising books, redrawing pictures.
#[tauri::command]
fn task_models(app: State<App>) -> Res<Vec<TaskModel>> {
    let conn = app.db.lock().unwrap();
    db::TASKS
        .iter()
        .map(|&task| {
            let chosen =
                db::setting(&conn, &format!("model.{task}"))?.is_some_and(|v| !v.is_empty());
            let m = db::model_for(&conn, task)?;
            Ok(TaskModel {
                task: task.into(),
                provider_id: m.as_ref().map(|(p, _)| p.id),
                model: m.map(|(_, m)| m).unwrap_or_default(),
                chosen,
            })
        })
        .collect()
}

#[tauri::command]
fn task_model_set(
    app: State<App>,
    task: String,
    provider_id: Option<i64>,
    model: String,
) -> Res<()> {
    if !db::TASKS.contains(&task.as_str()) {
        return Err(format!("unknown task {task}"));
    }
    let value = match provider_id {
        Some(id) if !model.trim().is_empty() => format!("{id}:{}", model.trim()),
        _ => String::new(),
    };
    db::set_setting(&app.db.lock().unwrap(), &format!("model.{task}"), &value)
}

/// Check a model: a one sentence translation for text models, a model list lookup for picture models (drawing a
/// test picture would cost money). `quick` is for trying many models in a row: one that is not there to answer
/// at once is not worth waiting for.
#[tauri::command]
async fn model_test(
    provider_id: i64,
    url: String,
    model: String,
    picture: bool,
    quick: Option<bool>,
) -> TestResult {
    let start = std::time::Instant::now();
    let client = OpenAiCompatible::new(url, secrets::get_key(provider_id), model.clone())
        .answers_within(if quick == Some(true) {
            30
        } else {
            celsus_core::provider::ANSWER_WAIT
        });
    if !picture {
        return translate_test(client);
    }
    let (ok, error) = match client.list_models() {
        Ok(list) if list.iter().any(|m| m == &model) => (true, None),
        Ok(_) => (
            false,
            Some(format!("{model} is not offered by this connection")),
        ),
        Err(e) => (false, Some(e.to_string())),
    };
    TestResult {
        ok,
        millis: start.elapsed().as_millis(),
        sample: if ok { model } else { String::new() },
        error,
        wait: None,
    }
}

// ---------- settings, profile, storage ----------

#[tauri::command]
fn settings_get(app: State<App>) -> Res<BTreeMap<String, String>> {
    let mut s = db::all_settings(&app.db.lock().unwrap())?;
    s.entry("fonts_dir".into())
        .or_insert_with(default_fonts_dir);
    s.entry("batch_chars".into())
        .or_insert_with(|| "5000".into());
    s.entry("answer_wait".into())
        .or_insert_with(|| celsus_core::provider::ANSWER_WAIT.to_string());
    s.entry("update_check".into()).or_insert_with(|| "1".into());
    Ok(s)
}

#[tauri::command]
fn settings_set(app: State<App>, values: BTreeMap<String, String>) -> Res<()> {
    let conn = app.db.lock().unwrap();
    for (k, v) in values {
        db::set_setting(&conn, &k, &v)?;
    }
    Ok(())
}

#[tauri::command]
fn usage_stats(app: State<App>) -> Res<db::Usage> {
    db::usage(&app.db.lock().unwrap())
}

#[derive(Serialize)]
struct StorageInfo {
    library: String,
    database: String,
    library_bytes: u64,
    cached_translations: i64,
    cached_chars: i64,
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

#[tauri::command]
async fn storage_info(handle: AppHandle) -> Res<StorageInfo> {
    let app = handle.state::<App>();
    let (n, chars) = db::cache_size(&app.db.lock().unwrap())?;
    Ok(StorageInfo {
        library: app.library.display().to_string(),
        database: app.db_path.display().to_string(),
        library_bytes: dir_size(&app.library),
        cached_translations: n,
        cached_chars: chars,
    })
}

/// Clear the kept translations that no translated page uses. What the books in the library are written from
/// stays: without it, the next time a book is written its pages would go back to the original.
#[tauri::command]
async fn cache_clear(handle: AppHandle) -> Res<()> {
    let app = handle.state::<App>();
    let translated = db::all_translated_pages(&app.db.lock().unwrap())?;
    // Reading the books takes a while; the database is free in the meantime.
    let keep = celsus_store::assistant::keys_in_use(&app.library, &translated);
    db::clear_cache_except(&app.db.lock().unwrap(), &keep)?;
    Ok(())
}

// ---------- MCP connection ----------

/// The MCP server other apps start: the program beside this one.
fn mcp_command() -> PathBuf {
    let name = if cfg!(windows) {
        "celsus-mcp.exe"
    } else {
        "celsus-mcp"
    };
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from(name));
    // An AppImage is mounted somewhere else every time it runs, so other apps are given a copy that stays put.
    if cfg!(target_os = "linux") && std::env::var_os("APPIMAGE").is_some() {
        static KEPT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        let kept = KEPT.get_or_init(|| {
            let to = celsus_store::data_dir().join("bin").join(name);
            keep_copy(&beside, &to).ok().map(|()| to)
        });
        if let Some(kept) = kept {
            return kept.clone();
        }
    }
    beside
}

/// Make `to` a copy of the program `from`, leaving it alone when it already is one. The copy is written beside
/// `to` and moved into place, so an app running the old one is not disturbed.
fn keep_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    let new = std::fs::read(from)?;
    if std::fs::read(to).is_ok_and(|old| old == new) {
        return Ok(());
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut part = to.as_os_str().to_owned();
    part.push(".new");
    let part = PathBuf::from(part);
    // Copying keeps the permission to run it.
    std::fs::copy(from, &part)?;
    std::fs::rename(&part, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

#[derive(Serialize)]
struct McpInfo {
    command: String,
    command_exists: bool,
    /// Apps that can use the server, and whether Celsus is set up in each.
    clients: Vec<mcp_clients::ClientInfo>,
    /// The entry most apps take, for any app not listed.
    generic_snippet: String,
    claude_code_command: String,
}

#[tauri::command]
fn mcp_info() -> McpInfo {
    let command = mcp_command().display().to_string();
    let generic = serde_json::json!({ "mcpServers": { mcp_clients::NAME: { "command": command, "args": [] } } });
    McpInfo {
        command_exists: mcp_command().exists(),
        clients: mcp_clients::list(&command),
        generic_snippet: serde_json::to_string_pretty(&generic).unwrap_or_default(),
        claude_code_command: format!("claude mcp add celsus -- \"{command}\""),
        command,
    }
}

/// Add Celsus to an app's MCP servers, keeping everything else in its settings.
#[tauri::command]
fn mcp_client_add(id: String) -> Res<String> {
    mcp_clients::add(&id, &mcp_command().display().to_string())
}

#[tauri::command]
fn mcp_client_remove(id: String) -> Res<()> {
    mcp_clients::remove(&id)
}

/// Open a web page (a provider's key page, a download page) in the default browser. Only https links.
#[tauri::command]
fn open_url(url: String) -> Res<()> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err("Only https links can be opened.".into());
    }
    #[cfg(windows)]
    let child = std::process::Command::new("explorer").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let child = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let child = std::process::Command::new("xdg-open").arg(&url).spawn();
    child.map(|_| ()).map_err(|e| e.to_string())
}

/// The languages books can be translated into, with their names and writing direction.
#[tauri::command]
fn languages() -> &'static [celsus_core::languages::LanguageInfo] {
    celsus_core::languages::LANGUAGES
}

#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

/// A newer release, and whether this copy of the app can put it in place by itself.
#[derive(Serialize)]
struct Update {
    #[serde(flatten)]
    release: celsus_core::release::Release,
    installs_itself: bool,
}

/// The Windows and macOS apps and the AppImage can replace themselves. A package installed through the system's
/// package manager (.deb, .rpm) is the system's to update.
fn installs_itself() -> bool {
    cfg!(any(windows, target_os = "macos")) || std::env::var_os("APPIMAGE").is_some()
}

/// The newest release of Celsus on GitHub, if it is newer than this one.
#[tauri::command]
async fn update_check() -> Res<Option<Update>> {
    let release = celsus_core::release::latest("byte-me-pls/celsus", env!("CARGO_PKG_VERSION"))?;
    Ok(release.map(|release| Update {
        release,
        installs_itself: installs_itself(),
    }))
}

/// Download the newest release, check that it is signed with the project's key, put it in place and start the
/// app again. A job that is running is taken up again after the restart.
#[tauri::command]
async fn update_install(handle: AppHandle) -> Res<()> {
    use tauri_plugin_updater::UpdaterExt;
    let update = handle
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("There is no newer version to install.")?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    handle.restart()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // The same folder the MCP server uses, so both see one library.
            let library = celsus_store::library_dir();
            let db_path = celsus_store::database_path();
            let conn = celsus_store::open().map_err(std::io::Error::other)?;
            if let Err(e) = seed_provider(&conn) {
                eprintln!("could not set up the first provider: {e}");
            }
            let runner = jobs::start(app.handle().clone(), db_path.clone(), library.clone());
            app.manage(App {
                db: Mutex::new(conn),
                library,
                db_path,
                runner,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library_list,
            book_get,
            pick_pdfs,
            books_add,
            book_update,
            book_delete,
            book_files,
            cover_save,
            book_pages,
            book_estimate,
            book_recognise,
            page_images,
            image_original,
            image_redraw,
            image_restore,
            task_models,
            task_model_set,
            model_test,
            save_output,
            read_file,
            job_create,
            jobs_list,
            job_cancel,
            job_retry,
            providers_list,
            provider_save,
            provider_delete,
            provider_default,
            provider_test,
            provider_models,
            provider_check,
            settings_get,
            settings_set,
            usage_stats,
            storage_info,
            cache_clear,
            mcp_info,
            mcp_client_add,
            mcp_client_remove,
            app_version,
            update_check,
            update_install,
            languages,
            open_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running Celsus");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kept_server_follows_the_one_beside_the_app() {
        let dir = std::env::temp_dir().join(format!("celsus-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("celsus-mcp"), dir.join("bin").join("celsus-mcp"));
        std::fs::write(&from, b"first build").unwrap();
        keep_copy(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"first build");
        // Unchanged: the copy is not written again.
        let written = std::fs::metadata(&to).unwrap().modified().unwrap();
        keep_copy(&from, &to).unwrap();
        assert_eq!(std::fs::metadata(&to).unwrap().modified().unwrap(), written);
        // A newer app brings a newer server.
        std::fs::write(&from, b"second build").unwrap();
        keep_copy(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"second build");
        assert!(!dir.join("bin").join("celsus-mcp.new").exists());
        // Nothing to copy: an error, and the copy that is there stays.
        std::fs::remove_file(&from).unwrap();
        assert!(keep_copy(&from, &to).is_err());
        assert_eq!(std::fs::read(&to).unwrap(), b"second build");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
