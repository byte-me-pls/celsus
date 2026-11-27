//! The Celsus library on disk, shared by the desktop app and the MCP server: a SQLite database and a folder with
//! one sub-folder per book.

pub mod assistant;
pub mod db;
pub mod library;
pub mod pictures;
pub mod secrets;

use std::path::PathBuf;

/// The app's identifier; it names the data folder on every system.
pub const APP_ID: &str = "io.github.byte-me-pls.celsus";

/// The data folder: `CELSUS_DATA_DIR`, or the per-user application data folder (the same one the desktop app uses).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CELSUS_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
    };
    base.join(APP_ID)
}

pub fn database_path() -> PathBuf {
    data_dir().join("celsus.db")
}

/// Where a backup asked to be restored is named, for the next start: the database cannot be replaced while it
/// is open.
pub fn restore_marker() -> PathBuf {
    data_dir().join("restore-from.txt")
}

/// Copy a folder and everything in it.
pub fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Make a backup of the library in a new folder inside `parent`: the database, written whole while it is in
/// use, and every book with its translations. Keys are not in it: they stay in the system's key store.
pub fn back_up(conn: &rusqlite::Connection, parent: &std::path::Path) -> Result<PathBuf, String> {
    let stamp = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Days since 1970 to a date, for a folder name people can read.
        let days = (secs / 86_400) as i64;
        let (y, m, d) = civil(days);
        format!("{y:04}-{m:02}-{d:02}")
    };
    let mut dir = parent.join(format!("Celsus backup {stamp}"));
    let mut n = 2;
    while dir.exists() {
        dir = parent.join(format!("Celsus backup {stamp} ({n})"));
        n += 1;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let db = dir.join("celsus.db");
    conn.execute("VACUUM INTO ?", [db.display().to_string()])
        .map_err(|e| e.to_string())?;
    copy_dir(&library_dir(), &dir.join("library")).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Whether `dir` is a backup made by [`back_up`].
pub fn is_backup(dir: &std::path::Path) -> bool {
    dir.join("celsus.db").is_file() && dir.join("library").is_dir()
}

/// Put a backup in place of the library, if one was asked for at the last start: what was there is kept
/// beside it, in a folder named for the time, until the user removes it. Runs before the database is opened.
pub fn restore_if_asked() -> Result<(), String> {
    let marker = restore_marker();
    let Ok(from) = std::fs::read_to_string(&marker) else {
        return Ok(());
    };
    let _ = std::fs::remove_file(&marker);
    let from = PathBuf::from(from.trim());
    if !is_backup(&from) {
        return Err(format!("{} is not a Celsus backup", from.display()));
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let before = data_dir().join(format!("before-restore-{secs}"));
    std::fs::create_dir_all(&before).map_err(|e| e.to_string())?;
    for name in ["celsus.db", "celsus.db-wal", "celsus.db-shm"] {
        let p = data_dir().join(name);
        if p.exists() {
            std::fs::rename(&p, before.join(name)).map_err(|e| e.to_string())?;
        }
    }
    if library_dir().exists() {
        std::fs::rename(library_dir(), before.join("library")).map_err(|e| e.to_string())?;
    }
    std::fs::copy(from.join("celsus.db"), database_path()).map_err(|e| e.to_string())?;
    copy_dir(&from.join("library"), &library_dir()).map_err(|e| e.to_string())?;
    Ok(())
}

/// The date of a day counted from 1970-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

pub fn library_dir() -> PathBuf {
    data_dir().join("library")
}

pub fn default_fonts_dir() -> String {
    if cfg!(windows) {
        r"C:\Windows\Fonts".into()
    } else if cfg!(target_os = "macos") {
        // The fonts macOS ships, `Supplemental` (Arial, Times New Roman) included.
        "/System/Library/Fonts".into()
    } else {
        "/usr/share/fonts".into()
    }
}

/// Open the database, creating the folders on first use.
pub fn open() -> db::Result<rusqlite::Connection> {
    std::fs::create_dir_all(library_dir()).map_err(|e| e.to_string())?;
    let conn = db::open(&database_path())?;
    library::settle_language_names(&library_dir());
    Ok(conn)
}
