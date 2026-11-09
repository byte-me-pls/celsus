//! Adding Celsus's MCP server to the chat and coding apps that can use one.
//!
//! Each app keeps its MCP servers in its own settings file. Celsus edits only its own entry there and leaves
//! everything else as it was; the file is backed up before the first change, and a file it cannot read safely
//! (JSON with comments, for example) is left alone with a message saying how to add the entry by hand.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};

/// The name Celsus's entry has in every app.
pub const NAME: &str = "celsus";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Format {
    /// A JSON file with servers under `key`. `typed` entries carry `"type": "stdio"`.
    Json { key: &'static str, typed: bool },
    /// OpenAI Codex's `config.toml`, `[mcp_servers.<name>]` tables.
    CodexToml,
}

struct Client {
    id: &'static str,
    name: &'static str,
    /// What it is, shown under the name.
    about: &'static str,
    format: Format,
    /// The settings file, from the home folder and the app data folder.
    file: fn(&Path, &Path) -> PathBuf,
    /// A folder whose presence shows the app is installed.
    found: fn(&Path, &Path) -> PathBuf,
}

const CLIENTS: &[Client] = &[
    Client {
        id: "claude-desktop",
        name: "Claude Desktop",
        about: "Anthropic",
        format: Format::Json {
            key: "mcpServers",
            typed: false,
        },
        file: |_, data| data.join("Claude").join("claude_desktop_config.json"),
        found: |_, data| data.join("Claude"),
    },
    Client {
        id: "claude-code",
        name: "Claude Code",
        about: "Anthropic · terminal, IDE",
        format: Format::Json {
            key: "mcpServers",
            typed: true,
        },
        file: |home, _| home.join(".claude.json"),
        found: |home, _| home.join(".claude.json"),
    },
    Client {
        id: "codex",
        name: "OpenAI Codex",
        about: "OpenAI · app, CLI, IDE",
        format: Format::CodexToml,
        file: |home, _| home.join(".codex").join("config.toml"),
        found: |home, _| home.join(".codex"),
    },
    Client {
        id: "gemini-cli",
        name: "Gemini CLI",
        about: "Google",
        format: Format::Json {
            key: "mcpServers",
            typed: false,
        },
        file: |home, _| home.join(".gemini").join("settings.json"),
        found: |home, _| home.join(".gemini"),
    },
    Client {
        id: "antigravity",
        name: "Antigravity",
        about: "Google",
        format: Format::Json {
            key: "mcpServers",
            typed: false,
        },
        file: |home, _| {
            home.join(".gemini")
                .join("antigravity")
                .join("mcp_config.json")
        },
        found: |home, _| home.join(".gemini").join("antigravity"),
    },
    Client {
        id: "vscode",
        name: "VS Code",
        about: "GitHub Copilot",
        format: Format::Json {
            key: "servers",
            typed: true,
        },
        file: |_, data| data.join("Code").join("User").join("mcp.json"),
        found: |_, data| data.join("Code").join("User"),
    },
    Client {
        id: "cursor",
        name: "Cursor",
        about: "Anysphere",
        format: Format::Json {
            key: "mcpServers",
            typed: true,
        },
        file: |home, _| home.join(".cursor").join("mcp.json"),
        found: |home, _| home.join(".cursor"),
    },
    Client {
        id: "windsurf",
        name: "Windsurf",
        about: "Cognition",
        format: Format::Json {
            key: "mcpServers",
            typed: false,
        },
        file: |home, _| {
            home.join(".codeium")
                .join("windsurf")
                .join("mcp_config.json")
        },
        found: |home, _| home.join(".codeium").join("windsurf"),
    },
    Client {
        id: "cline",
        name: "Cline",
        about: "VS Code extension",
        format: Format::Json {
            key: "mcpServers",
            typed: false,
        },
        file: |_, data| {
            data.join("Code")
                .join("User")
                .join("globalStorage")
                .join("saoudrizwan.claude-dev")
                .join("settings")
                .join("cline_mcp_settings.json")
        },
        found: |_, data| {
            data.join("Code")
                .join("User")
                .join("globalStorage")
                .join("saoudrizwan.claude-dev")
        },
    },
    Client {
        id: "lmstudio",
        name: "LM Studio",
        about: "Local models",
        format: Format::Json {
            key: "mcpServers",
            typed: false,
        },
        file: |home, _| home.join(".lmstudio").join("mcp.json"),
        found: |home, _| home.join(".lmstudio"),
    },
];

/// The folders the apps keep their settings in: the home folder and the per-user app data folder.
fn folders() -> Option<(PathBuf, PathBuf)> {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)?;
    let data = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)?
    } else if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
    };
    Some((home, data))
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Celsus is set up with this copy of the server.
    Added,
    /// Celsus is set up but points at another copy (an older build, a moved folder).
    Outdated,
    NotAdded,
    /// The settings file cannot be read safely (comments, broken JSON).
    Unreadable,
}

#[derive(Serialize)]
pub struct ClientInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub about: &'static str,
    pub file: String,
    /// The app seems to be installed on this computer.
    pub found: bool,
    pub state: State,
    /// What to paste by hand.
    pub snippet: String,
}

/// The entry an app gets for `command`.
fn entry(format: Format, command: &str) -> Value {
    match format {
        Format::Json { typed: true, .. } => {
            json!({"type": "stdio", "command": command, "args": []})
        }
        _ => json!({"command": command, "args": []}),
    }
}

fn snippet(format: Format, command: &str) -> String {
    match format {
        Format::Json { key, .. } => {
            let mut servers = Map::new();
            servers.insert(NAME.into(), entry(format, command));
            serde_json::to_string_pretty(&json!({ key: servers })).unwrap_or_default()
        }
        Format::CodexToml => toml_block(command),
    }
}

/// Read a JSON settings file: missing or empty is an empty object.
fn read_json(text: &str) -> Result<Map<String, Value>, String> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("the file is not a JSON object".into()),
        Err(e) => Err(format!("the file is not plain JSON ({e})")),
    }
}

/// The command of Celsus's entry in a JSON settings file, if it has one.
fn json_command(doc: &Map<String, Value>, key: &str) -> Option<String> {
    doc.get(key)?
        .get(NAME)?
        .get("command")?
        .as_str()
        .map(str::to_owned)
}

/// Put Celsus's entry in a JSON settings text, keeping every other entry and key in place.
fn json_add(text: &str, format: Format, command: &str) -> Result<String, String> {
    let Format::Json { key, .. } = format else {
        unreachable!()
    };
    let mut doc = read_json(text)?;
    let servers = doc.entry(key).or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(format!("\"{key}\" is not an object"));
    };
    servers.insert(NAME.into(), entry(format, command));
    serde_json::to_string_pretty(&Value::Object(doc)).map_err(|e| e.to_string())
}

fn json_remove(text: &str, key: &str) -> Result<String, String> {
    let mut doc = read_json(text)?;
    if let Some(Value::Object(servers)) = doc.get_mut(key) {
        servers.remove(NAME);
    }
    serde_json::to_string_pretty(&Value::Object(doc)).map_err(|e| e.to_string())
}

/// A TOML string for a path: literal (no escapes) unless it contains a single quote.
fn toml_string(s: &str) -> String {
    if s.contains('\'') || s.contains('\n') {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("'{s}'")
    }
}

fn toml_block(command: &str) -> String {
    format!(
        "[mcp_servers.{NAME}]\ncommand = {}\nargs = []\n",
        toml_string(command)
    )
}

/// Whether a TOML header line opens Celsus's table or one of its sub-tables.
fn is_celsus_header(line: &str) -> bool {
    let l = line.trim();
    let Some(inner) = l.strip_prefix('[').and_then(|r| r.split(']').next()) else {
        return false;
    };
    let inner = inner.replace(['"', '\''], "");
    let inner = inner.trim();
    inner == format!("mcp_servers.{NAME}") || inner.starts_with(&format!("mcp_servers.{NAME}."))
}

/// Remove Celsus's tables from a Codex `config.toml`, keeping every other line as it was.
fn toml_remove(text: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        let header = line.trim_start().starts_with('[');
        if header {
            skipping = is_celsus_header(line);
        }
        if !skipping {
            out.push(line);
        }
    }
    // Drop the blank lines a removed table leaves at the end.
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

fn toml_add(text: &str, command: &str) -> String {
    let mut s = toml_remove(text);
    if !s.is_empty() {
        s.push('\n');
    }
    s.push_str(&toml_block(command));
    s
}

fn toml_command(text: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t.replace(['"', '\''], "") == format!("[mcp_servers.{NAME}]");
            continue;
        }
        if inside && let Some(v) = t.strip_prefix("command") {
            let v = v.trim_start().strip_prefix('=')?.trim();
            return Some(if let Some(lit) = v.strip_prefix('\'') {
                lit.trim_end_matches('\'').to_owned()
            } else {
                v.trim_matches('"').replace("\\\\", "\\")
            });
        }
    }
    None
}

fn same_command(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn state_of(format: Format, file: &Path, command: &str) -> State {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return State::NotAdded,
    };
    let current = match format {
        Format::Json { key, .. } => match read_json(&text) {
            Ok(doc) => json_command(&doc, key),
            Err(_) => return State::Unreadable,
        },
        Format::CodexToml => toml_command(&text),
    };
    match current {
        Some(c) if same_command(&c, command) => State::Added,
        Some(_) => State::Outdated,
        None => State::NotAdded,
    }
}

pub fn list(command: &str) -> Vec<ClientInfo> {
    let Some((home, data)) = folders() else {
        return Vec::new();
    };
    CLIENTS
        .iter()
        .map(|c| {
            let file = (c.file)(&home, &data);
            ClientInfo {
                id: c.id,
                name: c.name,
                about: c.about,
                found: (c.found)(&home, &data).exists(),
                state: state_of(c.format, &file, command),
                file: file.display().to_string(),
                snippet: snippet(c.format, command),
            }
        })
        .collect()
}

fn client(id: &str) -> Result<(&'static Client, PathBuf), String> {
    let c = CLIENTS
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| format!("unknown app {id}"))?;
    let (home, data) = folders().ok_or("The home folder was not found.")?;
    Ok((c, (c.file)(&home, &data)))
}

/// `file` with `suffix` added to its name.
fn beside(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The text of a settings file; a file that is not there yet is empty. A file that is there but cannot be read
/// (in use, no permission, not text) is an error: treating it as empty would replace it with Celsus's entry
/// alone.
fn current(file: &Path) -> Result<String, String> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!(
            "{}: {e}. Add the entry by hand instead.",
            file.display()
        )),
    }
}

/// Replace a settings file whose text was `before`. Two copies are kept beside it: `.celsus-backup`, the file
/// before Celsus first touched it, and `.celsus-previous`, the file before this change. The new text is written
/// to a file of its own and moved into place, so the app never finds half a file.
fn write(file: &Path, before: &str, text: &str) -> Result<(), String> {
    if before == text {
        return Ok(());
    }
    let failed = |e: std::io::Error| format!("{}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    // A link is followed, so the file it points to is the one replaced.
    let target = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    if target.exists() {
        // The app may have saved its settings since they were read.
        if current(&target)? != before {
            return Err(format!(
                "{}: the file changed while Celsus was editing it. Try again.",
                file.display()
            ));
        }
        let first = beside(file, ".celsus-backup");
        if !first.exists() {
            std::fs::copy(&target, &first).map_err(failed)?;
        }
        std::fs::copy(&target, beside(file, ".celsus-previous")).map_err(failed)?;
    }
    let new = beside(&target, ".celsus-new");
    let written = (|| {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&new)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&new, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    written.map_err(failed)
}

fn edited(c: &Client, text: &str, file: &Path, add: Option<&str>) -> Result<String, String> {
    let result = match (c.format, add) {
        (Format::Json { .. }, Some(cmd)) => json_add(text, c.format, cmd),
        (Format::Json { key, .. }, None) => json_remove(text, key),
        (Format::CodexToml, Some(cmd)) => Ok(toml_add(text, cmd)),
        (Format::CodexToml, None) => Ok(toml_remove(text)),
    };
    result.map_err(|e| format!("{}: {e}. Add the entry by hand instead.", file.display()))
}

/// Add (or update) Celsus in an app. Returns the settings file.
pub fn add(id: &str, command: &str) -> Result<String, String> {
    let (c, file) = client(id)?;
    let before = current(&file)?;
    let text = edited(c, &before, &file, Some(command))?;
    write(&file, &before, &text)?;
    Ok(file.display().to_string())
}

pub fn remove(id: &str) -> Result<(), String> {
    let (c, file) = client(id)?;
    if !file.exists() {
        return Ok(());
    }
    let before = current(&file)?;
    let text = edited(c, &before, &file, None)?;
    write(&file, &before, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = r"C:\Apps\Celsus\celsus-mcp.exe";

    #[test]
    fn json_keeps_other_servers_and_keys_in_order() {
        let before =
            r#"{"theme":"dark","mcpServers":{"unityMCP":{"command":"uvx","args":["x"]}},"zoom":2}"#;
        let fmt = Format::Json {
            key: "mcpServers",
            typed: false,
        };
        let after = json_add(before, fmt, CMD).unwrap();
        let doc: Value = serde_json::from_str(&after).unwrap();
        assert_eq!(doc["mcpServers"]["unityMCP"]["args"][0], "x");
        assert_eq!(
            doc["mcpServers"]["celsus"],
            json!({"command": CMD, "args": []})
        );
        let keys: Vec<&String> = doc.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["theme", "mcpServers", "zoom"],
            "keys keep their order"
        );
        // Adding twice changes nothing more.
        assert_eq!(json_add(&after, fmt, CMD).unwrap(), after);
        // Removing leaves the rest.
        let removed: Value =
            serde_json::from_str(&json_remove(&after, "mcpServers").unwrap()).unwrap();
        assert!(removed["mcpServers"].get("celsus").is_none());
        assert!(removed["mcpServers"].get("unityMCP").is_some());
    }

    #[test]
    fn vs_code_style_entries_say_stdio_under_servers() {
        let fmt = Format::Json {
            key: "servers",
            typed: true,
        };
        let doc: Value = serde_json::from_str(&json_add("", fmt, CMD).unwrap()).unwrap();
        assert_eq!(doc["servers"]["celsus"]["type"], "stdio");
        assert_eq!(doc["servers"]["celsus"]["command"], CMD);
    }

    #[test]
    fn files_that_are_not_plain_json_are_left_alone() {
        let fmt = Format::Json {
            key: "mcpServers",
            typed: false,
        };
        assert!(json_add("{\n // my servers\n}", fmt, CMD).is_err());
        assert!(json_add("[1,2]", fmt, CMD).is_err());
        assert!(json_add(r#"{"mcpServers": 3}"#, fmt, CMD).is_err());
        // A byte order mark is fine.
        assert!(json_add("\u{feff}{}", fmt, CMD).is_ok());
    }

    #[test]
    fn codex_tables_are_added_replaced_and_removed() {
        let before = "model = 'gpt-5.5'\n\n[mcp_servers.unityMCP]\ncommand = 'uvx'\nargs = []\n\n[mcp_servers.celsus]\ncommand = 'old.exe'\n\n[mcp_servers.celsus.env]\nX = '1'\n\n[features]\nweb = true\n";
        let after = toml_add(before, CMD);
        assert!(after.contains("[mcp_servers.unityMCP]\ncommand = 'uvx'"));
        assert!(after.contains("[features]\nweb = true"));
        assert!(!after.contains("old.exe") && !after.contains("[mcp_servers.celsus.env]"));
        assert!(after.ends_with(&format!(
            "[mcp_servers.celsus]\ncommand = '{CMD}'\nargs = []\n"
        )));
        assert_eq!(toml_command(&after).as_deref(), Some(CMD));
        assert_eq!(toml_add(&after, CMD), after, "adding again changes nothing");
        let removed = toml_remove(&after);
        assert!(!removed.contains("celsus"));
        assert!(removed.contains("[mcp_servers.unityMCP]") && removed.contains("[features]"));
    }

    #[test]
    fn codex_paths_with_quotes_use_escaped_strings() {
        let odd = r"C:\Ali's\celsus-mcp.exe";
        let t = toml_add("", odd);
        assert!(
            t.contains(r#"command = "C:\\Ali's\\celsus-mcp.exe""#),
            "{t}"
        );
        assert_eq!(toml_command(&t).as_deref(), Some(odd));
        assert!(is_celsus_header("[mcp_servers.\"celsus\"]"));
        assert!(!is_celsus_header("[mcp_servers.celsus-other]"));
    }

    #[test]
    fn state_follows_the_file() {
        let dir = std::env::temp_dir().join(format!("celsus-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fmt = Format::Json {
            key: "mcpServers",
            typed: false,
        };
        let file = dir.join("config.json");
        assert_eq!(state_of(fmt, &file, CMD), State::NotAdded);
        std::fs::write(&file, json_add("", fmt, r"C:\old\celsus-mcp.exe").unwrap()).unwrap();
        assert_eq!(state_of(fmt, &file, CMD), State::Outdated);
        std::fs::write(&file, json_add("", fmt, CMD).unwrap()).unwrap();
        assert_eq!(state_of(fmt, &file, CMD), State::Added);
        std::fs::write(&file, "{ // comment\n}").unwrap();
        assert_eq!(state_of(fmt, &file, CMD), State::Unreadable);
        let toml = dir.join("config.toml");
        std::fs::write(&toml, toml_add("", CMD)).unwrap();
        assert_eq!(state_of(Format::CodexToml, &toml, CMD), State::Added);
        // Writing keeps the first original as a backup, and the version before each change.
        write(&file, "{ // comment\n}", "{}").unwrap();
        write(&file, "{}", "{\"a\":1}").unwrap();
        let backup = std::fs::read_to_string(dir.join("config.json.celsus-backup")).unwrap();
        assert_eq!(backup, "{ // comment\n}");
        let previous = std::fs::read_to_string(dir.join("config.json.celsus-previous")).unwrap();
        assert_eq!(previous, "{}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"a\":1}");
        assert!(!dir.join("config.json.celsus-new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_replaced() {
        let dir = std::env::temp_dir().join(format!("celsus-mcp-safe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Not text: reading fails, and nothing is taken to be empty.
        let file = dir.join("settings.json");
        let bytes = [b'{', 0xFF, 0xFE, b'}'];
        std::fs::write(&file, bytes).unwrap();
        assert!(current(&file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        // A file that is not there is simply empty.
        assert_eq!(current(&dir.join("none.json")).unwrap(), "");
        // Saved by its app after Celsus read it: the newer version stays.
        let live = dir.join("live.json");
        std::fs::write(&live, "{\"theirs\":2}").unwrap();
        assert!(write(&live, "{\"theirs\":1}", "{\"ours\":1}").is_err());
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "{\"theirs\":2}");
        assert!(!dir.join("live.json.celsus-backup").exists());
        // Nothing to change: nothing is written, not even a backup.
        write(&live, "{\"theirs\":2}", "{\"theirs\":2}").unwrap();
        assert!(!dir.join("live.json.celsus-backup").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_app_has_a_snippet_and_unique_id() {
        let mut ids = std::collections::BTreeSet::new();
        for c in CLIENTS {
            assert!(ids.insert(c.id), "{} twice", c.id);
            assert!(snippet(c.format, CMD).contains("celsus"));
        }
    }
}
