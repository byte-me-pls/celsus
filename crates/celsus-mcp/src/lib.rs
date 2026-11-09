//! Celsus MCP server.
//!
//! Lets an assistant in a chat (Claude Desktop, Claude Code or any MCP client) translate books in the Celsus
//! library: Celsus hands out the text block by block, the assistant translates it, and Celsus checks the answers
//! and writes the translated pages into the PDF without changing anything else on them.
//!
//! The server speaks JSON-RPC 2.0 over standard input and output, one message per line (see `main.rs`).

use std::path::Path;

use celsus_core::Language;
use celsus_store::{assistant, db, library, pictures};
use serde_json::{Value, json};

const PROTOCOL: &str = "2025-06-18";

const SERVER_INSTRUCTIONS: &str = "Celsus is a local book translation library. To translate a book: find it with \
list_books (or add a PDF with add_book), then repeat get_text_to_translate and submit_translations until no blocks \
are left. Translate the blocks yourself; Celsus checks each translation, returns the ones it rejects with the reason, \
and writes finished pages into the translated PDF. get_output gives the file.

Covers and other pictures with text in them are redrawn, not typed over. To translate a cover: list_pictures finds \
it, get_picture gives you the image. If you can generate images, draw it yourself: the same picture with every text \
translated, the artwork, colours and lettering style unchanged, the same proportions. Save it as a PNG or JPEG file \
and send it with replace_picture (file_path, or image_base64). Celsus puts it into the translated PDF. Only if you \
cannot generate images, use redraw_picture, which asks the picture model chosen in Celsus to draw it. \
restore_picture brings the original back.";

fn tools() -> Value {
    json!([
        {
            "name": "list_books",
            "annotations": { "readOnlyHint": true },
            "description": "List the books in the Celsus library with their page counts and how many pages are translated into each language.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "add_book",
            "description": "Add a PDF file from this computer to the Celsus library.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Absolute path of the PDF file." } },
                "required": ["path"]
            }
        },
        {
            "name": "book_info",
            "annotations": { "readOnlyHint": true },
            "description": "Details of one book; with a language, which pages are already translated into it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "target_language": { "type": "string", "description": "Language code of any major world language, e.g. tr, en, de, ar, hi, ja, zh-hans, pt-br." }
                },
                "required": ["book_id"]
            }
        },
        {
            "name": "get_text_to_translate",
            "annotations": { "readOnlyHint": true },
            "description": "The next blocks of text to translate from the unfinished pages, with instructions and surrounding context. Call again after submitting; an empty block list means the requested pages are done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "target_language": { "type": "string", "description": "Language code of any major world language, e.g. tr, ar, hi, ja, zh-hans, pt-br." },
                    "pages": { "type": "string", "description": "Pages to work on, e.g. \"10-12,15\". Omit for the whole book." },
                    "max_characters": { "type": "integer", "description": "About how much text to return (default 4000)." }
                },
                "required": ["book_id", "target_language"]
            }
        },
        {
            "name": "submit_translations",
            "description": "Send translations back. Each item needs the page and id from get_text_to_translate. Translations that fail Celsus's checks are returned with the problem; fix and send them again, or set accept_anyway if the source really calls for it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "target_language": { "type": "string" },
                    "translations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "page": { "type": "integer" },
                                "id": { "type": "string" },
                                "text": { "type": "string" }
                            },
                            "required": ["page", "id", "text"]
                        }
                    },
                    "accept_anyway": { "type": "boolean", "description": "Store translations even if a check flags them." }
                },
                "required": ["book_id", "target_language", "translations"]
            }
        },
        {
            "name": "list_pictures",
            "annotations": { "readOnlyHint": true },
            "description": "The pictures in a book (page 1 is usually the cover), largest first on each page, with whether each is already redrawn in the language. Also says which picture model Celsus would use to redraw them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "target_language": { "type": "string", "description": "Language code, e.g. tr, ar, ja." },
                    "pages": { "type": "string", "description": "Pages to look at, e.g. \"1\" or \"1-20\". Default: \"1\" (the cover). \"all\" for the whole book." }
                },
                "required": ["book_id"]
            }
        },
        {
            "name": "get_picture",
            "annotations": { "readOnlyHint": true },
            "description": "Look at a picture: returns the image itself. With target_language and redrawn=true, the redrawn version.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "page": { "type": "integer", "description": "Default 1 (the cover)." },
                    "picture_id": { "type": "string", "description": "From list_pictures; default the largest picture on the page." },
                    "target_language": { "type": "string" },
                    "redrawn": { "type": "boolean" }
                },
                "required": ["book_id"]
            }
        },
        {
            "name": "redraw_picture",
            "description": "Only for assistants that cannot generate images: Celsus asks the picture model chosen in its settings to redraw a picture (by default the cover) with its text translated. If you can generate images, draw the picture yourself and use replace_picture instead.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "target_language": { "type": "string", "description": "Language code, e.g. tr, ar, ja, zh-hans." },
                    "page": { "type": "integer", "description": "Default 1 (the cover)." },
                    "picture_id": { "type": "string", "description": "From list_pictures; default the largest picture on the page." },
                    "note": { "type": "string", "description": "Extra wishes, e.g. the exact translated title to use." }
                },
                "required": ["book_id", "target_language"]
            }
        },
        {
            "name": "replace_picture",
            "description": "The way to translate a cover or picture: look at it with get_picture, draw it yourself with every text translated (same artwork, colours, lettering style and proportions), save it as PNG or JPEG and send it here as a file path or base64. Celsus puts it into the translated PDF in place of the original.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "target_language": { "type": "string" },
                    "page": { "type": "integer", "description": "Default 1 (the cover)." },
                    "picture_id": { "type": "string", "description": "From list_pictures; default the largest picture on the page." },
                    "file_path": { "type": "string", "description": "Absolute path of a JPEG or PNG file." },
                    "image_base64": { "type": "string", "description": "The JPEG or PNG file as base64, instead of file_path." }
                },
                "required": ["book_id", "target_language"]
            }
        },
        {
            "name": "restore_picture",
            "description": "Put the original picture back in the translated PDF.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book_id": { "type": "integer" },
                    "target_language": { "type": "string" },
                    "page": { "type": "integer", "description": "Default 1 (the cover)." },
                    "picture_id": { "type": "string" }
                },
                "required": ["book_id", "target_language"]
            }
        },
        {
            "name": "get_output",
            "annotations": { "readOnlyHint": true },
            "description": "Where the translated PDF of a book is, and how many pages it has translated.",
            "inputSchema": {
                "type": "object",
                "properties": { "book_id": { "type": "integer" }, "target_language": { "type": "string" } },
                "required": ["book_id", "target_language"]
            }
        }
    ])
}

/// Parse "10-12,15" (or a JSON array of numbers) into page numbers.
fn parse_pages(v: Option<&Value>) -> Result<Vec<usize>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_u64()
                    .map(|n| n as usize)
                    .ok_or("pages must be numbers".into())
            })
            .collect(),
        Some(Value::Number(n)) => Ok(vec![n.as_u64().unwrap_or(0) as usize]),
        Some(Value::String(s)) => {
            let mut out = Vec::new();
            for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                match part.split_once('-') {
                    Some((a, b)) => {
                        let (a, b): (usize, usize) = (
                            a.trim().parse().map_err(|_| format!("bad page {a}"))?,
                            b.trim().parse().map_err(|_| format!("bad page {b}"))?,
                        );
                        if b < a || b - a > 5000 {
                            return Err(format!("bad range {part}"));
                        }
                        out.extend(a..=b);
                    }
                    None => out.push(part.parse().map_err(|_| format!("bad page {part}"))?),
                }
            }
            Ok(out)
        }
        _ => Err("pages must be text like \"10-12,15\"".into()),
    }
}

fn arg_i64(args: &Value, key: &str) -> Result<i64, String> {
    args[key]
        .as_i64()
        .ok_or_else(|| format!("{key} is required"))
}

fn arg_lang(args: &Value) -> Result<Language, String> {
    args["target_language"]
        .as_str()
        .ok_or("target_language is required")?
        .parse()
        .map_err(|e: celsus_core::language::InvalidLanguage| e.to_string())
}

/// A tool's answer: data, and pictures to show.
struct Output {
    value: Value,
    images: Vec<(Vec<u8>, &'static str)>,
}

impl From<Value> for Output {
    fn from(value: Value) -> Self {
        Output {
            value,
            images: Vec::new(),
        }
    }
}

/// Pictures larger than this are not sent back; models take only a few megabytes per image.
const MAX_PICTURE: usize = 8 << 20;

fn arg_page(args: &Value) -> Result<usize, String> {
    match &args["page"] {
        Value::Null => Ok(1),
        v => v
            .as_u64()
            .filter(|&n| n >= 1)
            .map(|n| n as usize)
            .ok_or_else(|| "page must be a page number".into()),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn picture_json(p: &pictures::Picture) -> Value {
    json!({ "picture_id": p.image, "page": p.page, "width": p.width, "height": p.height,
            "redrawn": p.redrawn.is_some() })
}

fn call_tool(name: &str, args: &Value) -> Result<Output, String> {
    if let Some(out) = picture_tool(name, args)? {
        return Ok(out);
    }
    call_text_tool(name, args).map(Output::from)
}

fn picture_tool(name: &str, args: &Value) -> Result<Option<Output>, String> {
    if !matches!(
        name,
        "list_pictures" | "get_picture" | "redraw_picture" | "replace_picture" | "restore_picture"
    ) {
        return Ok(None);
    }
    let conn = celsus_store::open()?;
    let lib = celsus_store::library_dir();
    let book = arg_i64(args, "book_id")?;
    let out: Output = match name {
        "list_pictures" => {
            let lang = arg_lang(args).ok();
            let pages = match args.get("pages") {
                Some(Value::String(s)) if s.trim().eq_ignore_ascii_case("all") => Vec::new(),
                None | Some(Value::Null) => vec![1],
                v => parse_pages(v)?,
            };
            let lang = lang.as_ref().map(|l| l.as_str()).unwrap_or("");
            let list = pictures::list(&lib, book, lang, &pages)?;
            let model = db::model_for(&conn, "image")?.map(|(p, m)| format!("{} / {m}", p.name));
            let mut out = json!({
                "pictures": list.iter().map(picture_json).collect::<Vec<_>>(),
                "picture_model": model,
            });
            if model.is_none() {
                out["note"] = json!(
                    "Draw the translated picture yourself and send it with replace_picture. (redraw_picture would need a \
                     picture model chosen in Celsus under Settings → Models; none is chosen.)"
                );
            }
            out.into()
        }
        "get_picture" => {
            let page = arg_page(args)?;
            let id = arg_str(args, "picture_id");
            let (image, bytes, mime) = if args["redrawn"].as_bool().unwrap_or(false) {
                let lang = arg_lang(args)?;
                let list = pictures::list(&lib, book, lang.as_str(), &[page])?;
                let p = list
                    .iter()
                    .find(|p| {
                        id.is_none_or(|i| {
                            library::parse_image_id(i) == library::parse_image_id(&p.image)
                        })
                    })
                    .ok_or("That picture is not on this page.")?;
                let file = p
                    .redrawn
                    .as_ref()
                    .ok_or("This picture is not redrawn in that language yet.")?;
                let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
                let mime = if bytes.starts_with(b"\x89PNG") {
                    "image/png"
                } else {
                    "image/jpeg"
                };
                (p.image.clone(), bytes, mime)
            } else {
                pictures::original(&lib, book, page, id)?
            };
            if bytes.len() > MAX_PICTURE {
                return Err(format!(
                    "The picture is {} MB, too large to send.",
                    bytes.len() >> 20
                ));
            }
            Output {
                value: json!({ "picture_id": image, "page": page, "bytes": bytes.len(), "type": mime }),
                images: vec![(bytes, mime)],
            }
        }
        "redraw_picture" => {
            let lang = arg_lang(args)?;
            let page = arg_page(args)?;
            let file = pictures::redraw(
                &conn,
                &lib,
                book,
                &lang,
                page,
                arg_str(args, "picture_id"),
                arg_str(args, "note"),
            )?;
            done(&lib, book, &lang, page, &file)
        }
        "replace_picture" => {
            let lang = arg_lang(args)?;
            let page = arg_page(args)?;
            let bytes = match (arg_str(args, "file_path"), arg_str(args, "image_base64")) {
                (Some(path), _) => std::fs::read(path).map_err(|e| format!("{path}: {e}"))?,
                (None, Some(b64)) => {
                    // A data URL is fine too.
                    let b64 = b64.split_once("base64,").map_or(b64, |(_, d)| d);
                    celsus_core::base64::decode(b64).ok_or("image_base64 is not valid base64")?
                }
                (None, None) => return Err("Give file_path or image_base64.".into()),
            };
            let file = pictures::place(
                &conn,
                &lib,
                book,
                &lang,
                page,
                arg_str(args, "picture_id"),
                &bytes,
            )?;
            done(&lib, book, &lang, page, &file)
        }
        _ => {
            let lang = arg_lang(args)?;
            let page = arg_page(args)?;
            pictures::restore(&conn, &lib, book, &lang, page, arg_str(args, "picture_id"))?;
            json!({ "restored": true, "page": page,
                    "output": library::output_file(&lib, book, lang.as_str()).display().to_string() })
            .into()
        }
    };
    Ok(Some(out))
}

fn done(lib: &Path, book: i64, lang: &Language, page: usize, file: &Path) -> Output {
    json!({
        "page": page,
        "redrawn_file": file.display().to_string(),
        "output": library::output_file(lib, book, lang.as_str()).display().to_string(),
        "next": "Look at it with get_picture (redrawn=true). If something is off, call redraw_picture again with a note."
    })
    .into()
}

fn call_text_tool(name: &str, args: &Value) -> Result<Value, String> {
    let conn = celsus_store::open()?;
    let lib = celsus_store::library_dir();
    match name {
        "list_books" => {
            let books = db::books(&conn)?;
            Ok(json!(books
                .iter()
                .map(|b| json!({ "book_id": b.id, "title": b.title, "author": b.author, "pages": b.pages,
                                 "translated_pages": b.translated, "busy": b.busy }))
                .collect::<Vec<_>>()))
        }
        "add_book" => {
            let path = args["path"].as_str().ok_or("path is required")?;
            let b = library::add_book(&conn, &lib, Path::new(path))?;
            Ok(json!({ "book_id": b.id, "title": b.title, "author": b.author, "pages": b.pages }))
        }
        "book_info" => {
            let b = db::book(&conn, arg_i64(args, "book_id")?)?;
            let mut out = json!({ "book_id": b.id, "title": b.title, "author": b.author, "subject": b.subject,
                                  "source_language": b.source_lang, "pages": b.pages, "translated_pages": b.translated });
            if let Ok(lang) = arg_lang(args) {
                let done = db::translated_pages(&conn, b.id, lang.as_str())?;
                out["done_pages"] = json!(done.iter().map(|(p, _)| p).collect::<Vec<_>>());
                out["pages_with_blocks_kept_original"] = json!(
                    done.iter()
                        .filter(|(_, k)| *k > 0)
                        .map(|(p, _)| p)
                        .collect::<Vec<_>>()
                );
            }
            Ok(out)
        }
        "get_text_to_translate" => {
            let book = arg_i64(args, "book_id")?;
            let lang = arg_lang(args)?;
            let pages = parse_pages(args.get("pages"))?;
            let max = args["max_characters"]
                .as_u64()
                .unwrap_or(4000)
                .clamp(500, 20000) as usize;
            let p = assistant::pending(&conn, &lib, book, &lang, &pages, max)?;
            Ok(serde_json::to_value(p).map_err(|e| e.to_string())?)
        }
        "submit_translations" => {
            let book = arg_i64(args, "book_id")?;
            let lang = arg_lang(args)?;
            let items: Vec<assistant::Submitted> =
                serde_json::from_value(args["translations"].clone())
                    .map_err(|e| format!("translations: {e}"))?;
            let accept = args["accept_anyway"].as_bool().unwrap_or(false);
            let r = assistant::submit(&conn, &lib, book, &lang, &items, accept)?;
            Ok(serde_json::to_value(r).map_err(|e| e.to_string())?)
        }
        "get_output" => {
            let book = arg_i64(args, "book_id")?;
            let lang = arg_lang(args)?;
            let file = library::output_file(&lib, book, lang.as_str());
            let pages = db::translated_pages(&conn, book, lang.as_str())?.len();
            Ok(
                json!({ "path": file.exists().then(|| file.display().to_string()), "translated_pages": pages }),
            )
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

/// Answer one JSON-RPC message; `None` for notifications.
pub fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => {
            let version = msg["params"]["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "celsus", "title": "Celsus", "version": env!("CARGO_PKG_VERSION") },
                "instructions": SERVER_INSTRUCTIONS
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            Ok(match call_tool(name, &args) {
                Ok(Output { value, images }) => {
                    let mut content = vec![json!({ "type": "text", "text": value.to_string() })];
                    content.extend(images.into_iter().map(|(bytes, mime)| {
                        json!({ "type": "image", "data": celsus_core::base64::encode(&bytes), "mimeType": mime })
                    }));
                    json!({ "content": content, "structuredContent": wrap(value), "isError": false })
                }
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            })
        }
        _ if id.is_none() => return None, // notifications need no answer
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    let id = id?;
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

/// Structured content must be an object.
fn wrap(v: Value) -> Value {
    if v.is_object() {
        v
    } else {
        json!({ "result": v })
    }
}
