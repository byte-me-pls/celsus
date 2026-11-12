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
