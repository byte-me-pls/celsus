//! The app's local database: library, translation jobs, cached translations, providers and settings.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS books (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    author TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL DEFAULT '',
    original_name TEXT NOT NULL,
    pages INTEGER NOT NULL,
    bytes INTEGER NOT NULL,
    source_lang TEXT NOT NULL DEFAULT '',
    has_cover INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL,
    opened_at INTEGER
);
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY,
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    target_lang TEXT NOT NULL,
    pages TEXT NOT NULL,
    status TEXT NOT NULL,
    done_pages INTEGER NOT NULL DEFAULT 0,
    total_pages INTEGER NOT NULL DEFAULT 0,
    replaced INTEGER NOT NULL DEFAULT 0,
    kept INTEGER NOT NULL DEFAULT 0,
    requests INTEGER NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    model TEXT NOT NULL DEFAULT '',
    error TEXT,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER
);
CREATE TABLE IF NOT EXISTS translated_pages (
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    lang TEXT NOT NULL,
    page INTEGER NOT NULL,
    kept INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (book_id, lang, page)
);
CREATE TABLE IF NOT EXISTS cache (key TEXT PRIMARY KEY, text TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS providers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    model TEXT NOT NULL,
    is_default INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(err)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(err)?;
    conn.execute_batch(SCHEMA).map_err(err)?;
    // Covers used to be a picture taken out of the first page, which was sometimes the wrong one. They are drawn
    // from the page now: forget the old ones once, and the app draws them again.
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(err)?;
    if version < 1 {
        conn.execute_batch("UPDATE books SET has_cover = 0; PRAGMA user_version = 1;")
            .map_err(err)?;
    }
    // A language used to be written as it was typed in one place and in its canonical case in another
    // ("zh-hans", "zh-Hans"), which made two translations out of one. It has one spelling now.
    if version < 2 {
        conn.execute_batch(
            "UPDATE OR REPLACE translated_pages SET lang = lower(lang) WHERE lang <> lower(lang);
             UPDATE jobs SET target_lang = lower(target_lang) WHERE target_lang <> lower(target_lang);
             PRAGMA user_version = 2;",
        )
        .map_err(err)?;
    }
    Ok(conn)
}

// ---------- books ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    pub id: i64,
    pub title: String,
    pub author: String,
    pub subject: String,
    pub original_name: String,
    pub pages: i64,
    pub bytes: i64,
    pub source_lang: String,
    pub has_cover: bool,
    pub added_at: i64,
    pub opened_at: Option<i64>,
    /// Translated pages per language, e.g. `{"tr": 12}`.
    #[serde(default)]
    pub translated: std::collections::BTreeMap<String, i64>,
    /// A job for this book is queued or running.
    #[serde(default)]
    pub busy: bool,
}

fn book_row(r: &rusqlite::Row) -> rusqlite::Result<Book> {
    Ok(Book {
        id: r.get(0)?,
        title: r.get(1)?,
        author: r.get(2)?,
        subject: r.get(3)?,
        original_name: r.get(4)?,
        pages: r.get(5)?,
        bytes: r.get(6)?,
        source_lang: r.get(7)?,
        has_cover: r.get::<_, i64>(8)? != 0,
        added_at: r.get(9)?,
        opened_at: r.get(10)?,
        translated: Default::default(),
        busy: false,
    })
}

const BOOK_COLUMNS: &str = "id, title, author, subject, original_name, pages, bytes, source_lang, has_cover, added_at, opened_at";

fn decorate(conn: &Connection, mut book: Book) -> Result<Book> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT lang, COUNT(*) FROM translated_pages WHERE book_id = ? GROUP BY lang",
        )
        .map_err(err)?;
    book.translated = stmt
        .query_map([book.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)?;
    book.busy = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE book_id = ? AND status IN ('queued', 'running'))",
            [book.id],
            |r| r.get::<_, i64>(0),
        )
        .map_err(err)?
        != 0;
    Ok(book)
}

pub fn books(conn: &Connection) -> Result<Vec<Book>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {BOOK_COLUMNS} FROM books ORDER BY COALESCE(opened_at, added_at) DESC"
        ))
        .map_err(err)?;
    let list: Vec<Book> = stmt
        .query_map([], book_row)
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)?;
    list.into_iter().map(|b| decorate(conn, b)).collect()
}

pub fn book(conn: &Connection, id: i64) -> Result<Book> {
    let b = conn
        .query_row(
            &format!("SELECT {BOOK_COLUMNS} FROM books WHERE id = ?"),
            [id],
            book_row,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => format!("There is no book with id {id}."),
            other => err(other),
        })?;
    decorate(conn, b)
}

pub fn insert_book(conn: &Connection, b: &Book) -> Result<i64> {
    conn.execute(
        "INSERT INTO books (title, author, subject, original_name, pages, bytes, source_lang, has_cover, added_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![b.title, b.author, b.subject, b.original_name, b.pages, b.bytes, b.source_lang, b.has_cover, now()],
    )
    .map_err(err)?;
    Ok(conn.last_insert_rowid())
}

pub fn set_cover(conn: &Connection, id: i64, has: bool) -> Result<()> {
    conn.execute(
        "UPDATE books SET has_cover = ? WHERE id = ?",
        params![has, id],
    )
    .map_err(err)?;
    Ok(())
}

pub fn update_book(
    conn: &Connection,
    id: i64,
    title: &str,
    author: &str,
    subject: &str,
    source_lang: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE books SET title = ?, author = ?, subject = ?, source_lang = ? WHERE id = ?",
        params![title, author, subject, source_lang, id],
    )
    .map_err(err)?;
    Ok(())
}

pub fn touch_book(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE books SET opened_at = ? WHERE id = ?",
        params![now(), id],
    )
    .map_err(err)?;
    Ok(())
}

pub fn delete_book(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM books WHERE id = ?", [id])
        .map_err(err)?;
    Ok(())
}

/// The one spelling of a language in the database and in file names: "zh-Hans" and "ZH-HANS" are "zh-hans".
pub fn lang_key(lang: &str) -> String {
    lang.trim().to_lowercase()
}

pub fn translated_pages(conn: &Connection, book: i64, lang: &str) -> Result<Vec<(i64, i64)>> {
    let lang = lang_key(lang);
    let mut stmt = conn
        .prepare(
            "SELECT page, kept FROM translated_pages WHERE book_id = ? AND lang = ? ORDER BY page",
        )
        .map_err(err)?;
    stmt.query_map(params![book, lang], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)
}

pub fn mark_page(conn: &Connection, book: i64, lang: &str, page: i64, kept: i64) -> Result<()> {
    let lang = lang_key(lang);
    conn.execute(
        "INSERT INTO translated_pages (book_id, lang, page, kept) VALUES (?, ?, ?, ?)
         ON CONFLICT(book_id, lang, page) DO UPDATE SET kept = excluded.kept",
        params![book, lang, page, kept],
    )
    .map_err(err)?;
    Ok(())
}

/// A page is not translated after all: its translation could not be written again.
pub fn unmark_page(conn: &Connection, book: i64, lang: &str, page: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM translated_pages WHERE book_id = ? AND lang = ? AND page = ?",
        params![book, lang_key(lang), page],
    )
    .map_err(err)?;
    Ok(())
}

// ---------- jobs ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub book_id: i64,
    pub book_title: String,
    pub target_lang: String,
    pub pages: Vec<i64>,
    pub status: String,
    pub done_pages: i64,
    pub total_pages: i64,
    pub replaced: i64,
    pub kept: i64,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub model: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

const JOB_SELECT: &str = "SELECT j.id, j.book_id, b.title, j.target_lang, j.pages, j.status, j.done_pages, j.total_pages,
    j.replaced, j.kept, j.requests, j.prompt_tokens, j.completion_tokens, j.model, j.error, j.created_at, j.started_at,
    j.finished_at FROM jobs j JOIN books b ON b.id = j.book_id";

fn job_row(r: &rusqlite::Row) -> rusqlite::Result<Job> {
    let pages: String = r.get(4)?;
    Ok(Job {
        id: r.get(0)?,
        book_id: r.get(1)?,
        book_title: r.get(2)?,
        target_lang: r.get(3)?,
        pages: serde_json::from_str(&pages).unwrap_or_default(),
        status: r.get(5)?,
        done_pages: r.get(6)?,
        total_pages: r.get(7)?,
        replaced: r.get(8)?,
        kept: r.get(9)?,
        requests: r.get(10)?,
        prompt_tokens: r.get(11)?,
        completion_tokens: r.get(12)?,
        model: r.get(13)?,
        error: r.get(14)?,
        created_at: r.get(15)?,
        started_at: r.get(16)?,
        finished_at: r.get(17)?,
    })
}

pub fn jobs(conn: &Connection, book: Option<i64>) -> Result<Vec<Job>> {
    let sql = match book {
        Some(_) => format!("{JOB_SELECT} WHERE j.book_id = ?1 ORDER BY j.id DESC"),
        None => format!("{JOB_SELECT} WHERE ?1 IS NULL ORDER BY j.id DESC LIMIT 200"),
    };
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    stmt.query_map([book], job_row)
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)
}

pub fn job(conn: &Connection, id: i64) -> Result<Job> {
    conn.query_row(&format!("{JOB_SELECT} WHERE j.id = ?"), [id], job_row)
        .map_err(err)
}

pub fn insert_job(conn: &Connection, book: i64, lang: &str, pages: &[i64]) -> Result<i64> {
    let lang = lang_key(lang);
    conn.execute(
        "INSERT INTO jobs (book_id, target_lang, pages, status, total_pages, created_at) VALUES (?, ?, ?, 'queued', ?, ?)",
        params![book, lang, serde_json::to_string(pages).map_err(err)?, pages.len() as i64, now()],
    )
    .map_err(err)?;
    Ok(conn.last_insert_rowid())
}

pub fn next_queued(conn: &Connection) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM jobs WHERE status = 'queued' ORDER BY id LIMIT 1",
        [],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

pub fn set_status(conn: &Connection, id: i64, status: &str, error: Option<&str>) -> Result<()> {
    let (started, finished) = match status {
        "running" => (Some(now()), None),
        "done" | "failed" | "cancelled" => (None, Some(now())),
        _ => (None, None),
    };
    conn.execute(
        "UPDATE jobs SET status = ?, error = ?, started_at = COALESCE(?, started_at), finished_at = ? WHERE id = ?",
        params![status, error, started, finished, id],
    )
    .map_err(err)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn job_counters(
    conn: &Connection,
    id: i64,
    done: i64,
    total: i64,
    replaced: i64,
    kept: i64,
    requests: i64,
    prompt: i64,
    completion: i64,
    model: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET done_pages = ?, total_pages = ?, replaced = ?, kept = ?, requests = ?, prompt_tokens = ?,
         completion_tokens = ?, model = ? WHERE id = ?",
        params![done, total, replaced, kept, requests, prompt, completion, model, id],
    )
    .map_err(err)?;
    Ok(())
}

/// Jobs left running when the app closed go back to the queue.
pub fn requeue_interrupted(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET status = 'queued' WHERE status = 'running'",
        [],
    )
    .map_err(err)?;
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Usage {
    pub books: i64,
    pub jobs: i64,
    pub pages: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub month_pages: i64,
    pub month_tokens: i64,
}

pub fn usage(conn: &Connection) -> Result<Usage> {
    let month_start = now() - 30 * 24 * 3600;
    conn.query_row(
        "SELECT (SELECT COUNT(*) FROM books), COUNT(*), COALESCE(SUM(done_pages), 0), COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(CASE WHEN created_at >= ?1 THEN done_pages END), 0),
                COALESCE(SUM(CASE WHEN created_at >= ?1 THEN prompt_tokens + completion_tokens END), 0)
         FROM jobs",
        [month_start],
        |r| {
            Ok(Usage {
                books: r.get(0)?,
                jobs: r.get(1)?,
                pages: r.get(2)?,
                prompt_tokens: r.get(3)?,
                completion_tokens: r.get(4)?,
                month_pages: r.get(5)?,
                month_tokens: r.get(6)?,
            })
        },
    )
    .map_err(err)
}

// ---------- providers ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub model: String,
    pub is_default: bool,
    /// Whether a key is stored for it (the key itself never leaves the backend).
    #[serde(default)]
    pub has_key: bool,
}

pub fn providers(conn: &Connection) -> Result<Vec<Provider>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, url, model, is_default FROM providers ORDER BY is_default DESC, id",
        )
        .map_err(err)?;
    stmt.query_map([], |r| {
        Ok(Provider {
            id: r.get(0)?,
            name: r.get(1)?,
            url: r.get(2)?,
            model: r.get(3)?,
            is_default: r.get::<_, i64>(4)? != 0,
            has_key: false,
        })
    })
    .map_err(err)?
    .collect::<rusqlite::Result<_>>()
    .map_err(err)
}

pub fn save_provider(conn: &Connection, p: &Provider) -> Result<i64> {
    let id = if p.id > 0 {
        conn.execute(
            "UPDATE providers SET name = ?, url = ?, model = ? WHERE id = ?",
            params![p.name, p.url, p.model, p.id],
        )
        .map_err(err)?;
        p.id
    } else {
        conn.execute(
            "INSERT INTO providers (name, url, model) VALUES (?, ?, ?)",
            params![p.name, p.url, p.model],
        )
        .map_err(err)?;
        conn.last_insert_rowid()
    };
    let any_default: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM providers WHERE is_default = 1",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    if p.is_default || any_default == 0 {
        set_default_provider(conn, id)?;
    }
    Ok(id)
}

pub fn set_default_provider(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("UPDATE providers SET is_default = (id = ?)", [id])
        .map_err(err)?;
    Ok(())
}

pub fn delete_provider(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM providers WHERE id = ?", [id])
        .map_err(err)?;
    conn.execute(
        "UPDATE providers SET is_default = 1 WHERE id = (SELECT MIN(id) FROM providers)
         AND NOT EXISTS (SELECT 1 FROM providers WHERE is_default = 1)",
        [],
    )
    .map_err(err)?;
    Ok(())
}

pub fn default_provider(conn: &Connection) -> Result<Option<Provider>> {
    Ok(providers(conn)?.into_iter().find(|p| p.is_default))
}

// ---------- which model does what ----------

/// The jobs a model can be chosen for.
pub const TASKS: [&str; 3] = ["translate", "metadata", "image"];

/// The connection and model chosen for a task (setting `model.<task>` = `"<connection id>:<model>"`). Translation
/// falls back to the default connection and its model; recognising books falls back to the translation model.
pub fn model_for(conn: &Connection, task: &str) -> Result<Option<(Provider, String)>> {
    let list = providers(conn)?;
    let chosen = setting(conn, &format!("model.{task}"))?.and_then(|v| {
        let (id, model) = v.split_once(':')?;
        let id: i64 = id.parse().ok()?;
        let p = list.iter().find(|p| p.id == id)?.clone();
        (!model.trim().is_empty()).then(|| (p, model.trim().to_owned()))
    });
    Ok(match (chosen, task) {
        (Some(c), _) => Some(c),
        (None, "translate") => list
            .into_iter()
            .find(|p| p.is_default)
            .filter(|p| !p.model.is_empty())
            .map(|p| {
                let m = p.model.clone();
                (p, m)
            }),
        (None, "metadata") => model_for(conn, "translate")?,
        (None, _) => None,
    })
}

// ---------- settings ----------

pub fn setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM settings WHERE key = ?", [key], |r| {
        r.get(0)
    })
    .optional()
    .map_err(err)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(err)?;
    Ok(())
}

pub fn all_settings(conn: &Connection) -> Result<std::collections::BTreeMap<String, String>> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM settings")
        .map_err(err)?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)
}

// ---------- translation cache ----------

/// The translation cache kept in the database, used by the translation pipeline.
pub struct DbCache<'c>(pub &'c Connection);

impl celsus_core::pipeline::Cache for DbCache<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0
            .query_row("SELECT text FROM cache WHERE key = ?", [key], |r| r.get(0))
            .ok()
    }

    fn put(&mut self, key: &str, translation: &str) {
        let _ = self.0.execute(
            "INSERT INTO cache (key, text) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET text = excluded.text",
            params![key, translation],
        );
    }
}

pub fn cache_size(conn: &Connection) -> Result<(i64, i64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(text)), 0) FROM cache",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(err)
}

/// Clear the kept translations, all but those under the keys in `keep`. Says how many went.
pub fn clear_cache_except(
    conn: &Connection,
    keep: &std::collections::HashSet<String>,
) -> Result<usize> {
    let keys: Vec<String> = conn
        .prepare("SELECT key FROM cache")
        .map_err(err)?
        .query_map([], |r| r.get(0))
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)?;
    let tx = conn.unchecked_transaction().map_err(err)?;
    let mut cleared = 0;
    for key in keys.iter().filter(|k| !keep.contains(*k)) {
        cleared += tx
            .execute("DELETE FROM cache WHERE key = ?", [key])
            .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    Ok(cleared)
}

/// Every translated page in the library: (book, language, pages).
pub fn all_translated_pages(conn: &Connection) -> Result<Vec<(i64, String, Vec<usize>)>> {
    let mut stmt = conn
        .prepare("SELECT book_id, lang, page FROM translated_pages ORDER BY book_id, lang, page")
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(err)?;
    let mut out: Vec<(i64, String, Vec<usize>)> = Vec::new();
    for row in rows {
        let (book, lang, page) = row.map_err(err)?;
        match out.last_mut() {
            Some((b, l, pages)) if *b == book && *l == lang => pages.push(page as usize),
            _ => out.push((book, lang, vec![page as usize])),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        open(Path::new(":memory:")).unwrap()
    }

    fn provider(conn: &Connection, name: &str, model: &str) -> i64 {
        save_provider(
            conn,
            &Provider {
                id: 0,
                name: name.into(),
                url: format!("https://{name}/v1"),
                model: model.into(),
                is_default: false,
                has_key: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn nothing_is_chosen_without_connections() {
        let c = conn();
        for task in TASKS {
            assert!(model_for(&c, task).unwrap().is_none());
        }
    }

    #[test]
    fn translation_falls_back_to_the_default_connection_and_book_details_to_translation() {
        let c = conn();
        let nvidia = provider(&c, "nvidia", "nemotron");
        let (p, m) = model_for(&c, "translate").unwrap().unwrap();
        assert_eq!((p.id, m.as_str()), (nvidia, "nemotron"));
        let (p, m) = model_for(&c, "metadata").unwrap().unwrap();
        assert_eq!((p.id, m.as_str()), (nvidia, "nemotron"));
        assert!(
            model_for(&c, "image").unwrap().is_none(),
            "pictures are never drawn by accident"
        );
    }

    #[test]
    fn a_chosen_model_wins_and_clearing_it_restores_the_fallback() {
        let c = conn();
        provider(&c, "nvidia", "nemotron");
        let openai = provider(&c, "openai", "");
        set_setting(&c, "model.translate", &format!("{openai}:gpt-5.5-mini")).unwrap();
        set_setting(&c, "model.image", &format!("{openai}:gpt-image-2")).unwrap();
        assert_eq!(
            model_for(&c, "translate").unwrap().unwrap().1,
            "gpt-5.5-mini"
        );
        assert_eq!(
            model_for(&c, "metadata").unwrap().unwrap().1,
            "gpt-5.5-mini"
        );
        assert_eq!(model_for(&c, "image").unwrap().unwrap().0.id, openai);
        set_setting(&c, "model.translate", "").unwrap();
        assert_eq!(model_for(&c, "translate").unwrap().unwrap().1, "nemotron");
    }

    #[test]
    fn a_choice_pointing_at_a_removed_connection_is_ignored() {
        let c = conn();
        provider(&c, "nvidia", "nemotron");
        let gone = provider(&c, "openai", "");
        set_setting(&c, "model.image", &format!("{gone}:gpt-image-2")).unwrap();
        delete_provider(&c, gone).unwrap();
        assert!(model_for(&c, "image").unwrap().is_none());
        set_setting(&c, "model.translate", "garbage").unwrap();
        assert_eq!(model_for(&c, "translate").unwrap().unwrap().1, "nemotron");
    }

    #[test]
    fn first_connection_becomes_the_default() {
        let c = conn();
        let a = provider(&c, "a", "x");
        let b = provider(&c, "b", "y");
        assert_eq!(default_provider(&c).unwrap().unwrap().id, a);
        delete_provider(&c, a).unwrap();
        assert_eq!(default_provider(&c).unwrap().unwrap().id, b);
    }

    #[test]
    fn a_language_has_one_spelling() {
        let c = conn();
        c.execute_batch(
            "INSERT INTO books (id, title, author, subject, original_name, pages, bytes, source_lang, has_cover, added_at)
             VALUES (1, 'A book', '', '', 'a.pdf', 10, 1, 'en', 0, 0);",
        )
        .unwrap();
        mark_page(&c, 1, "zh-hans", 3, 0).unwrap();
        mark_page(&c, 1, "zh-Hans", 4, 1).unwrap();
        // However it is written, it is the same translation.
        assert_eq!(
            translated_pages(&c, 1, "zh-Hans").unwrap(),
            [(3, 0), (4, 1)]
        );
        assert_eq!(
            translated_pages(&c, 1, "ZH-HANS").unwrap(),
            [(3, 0), (4, 1)]
        );
        let job = insert_job(&c, 1, "pt-BR", &[1]).unwrap();
        let lang: String = c
            .query_row("SELECT target_lang FROM jobs WHERE id = ?", [job], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(lang, "pt-br");
    }

    #[test]
    fn pages_written_under_another_spelling_are_found_after_an_update() {
        let file = std::env::temp_dir().join(format!("celsus-lang-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&file);
        {
            let c = open(&file).unwrap();
            // As an earlier version left it: the same language under two spellings.
            c.execute_batch(
                "INSERT INTO books (id, title, author, subject, original_name, pages, bytes, source_lang, has_cover, added_at)
                 VALUES (1, 'A book', '', '', 'a.pdf', 10, 1, 'en', 0, 0);
                 INSERT INTO translated_pages (book_id, lang, page, kept) VALUES (1, 'zh-Hans', 1, 0);
                 INSERT INTO translated_pages (book_id, lang, page, kept) VALUES (1, 'zh-hans', 1, 2);
                 INSERT INTO translated_pages (book_id, lang, page, kept) VALUES (1, 'zh-Hans', 2, 0);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let c = open(&file).unwrap();
        let pages: Vec<i64> = translated_pages(&c, 1, "zh-hans")
            .unwrap()
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(pages, [1, 2]);
        let spellings: i64 = c
            .query_row(
                "SELECT COUNT(DISTINCT lang) FROM translated_pages",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(spellings, 1);
        drop(c);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn clearing_the_cache_leaves_what_is_in_use() {
        let c = conn();
        for key in ["tr|one", "tr|two", "tr|three"] {
            c.execute("INSERT INTO cache (key, text) VALUES (?, 'x')", [key])
                .unwrap();
        }
        let keep = std::collections::HashSet::from(["tr|two".to_string()]);
        assert_eq!(clear_cache_except(&c, &keep).unwrap(), 2);
        let left: Vec<String> = c
            .prepare("SELECT key FROM cache")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(left, ["tr|two"]);
    }

    #[test]
    fn translated_pages_are_listed_by_book_and_language_and_can_be_unmarked() {
        let c = conn();
        c.execute_batch(
            "INSERT INTO books (id, title, author, subject, original_name, pages, bytes, source_lang, has_cover, added_at)
             VALUES (1, 'A book', '', '', 'a.pdf', 10, 1, 'en', 0, 0);",
        )
        .unwrap();
        mark_page(&c, 1, "tr", 2, 0).unwrap();
        mark_page(&c, 1, "tr", 5, 0).unwrap();
        mark_page(&c, 1, "de", 2, 0).unwrap();
        assert_eq!(
            all_translated_pages(&c).unwrap(),
            [
                (1, "de".to_string(), vec![2]),
                (1, "tr".to_string(), vec![2, 5])
            ]
        );
        unmark_page(&c, 1, "TR", 5).unwrap();
        assert_eq!(translated_pages(&c, 1, "tr").unwrap(), [(2, 0)]);
    }
}
