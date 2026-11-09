//! The library over MCP (Streamable HTTP): chat and coding apps translate books, look at covers and place the
//! pictures they draw, as the person whose key or sign-in they use.
//!
//! `POST /mcp` takes JSON-RPC 2.0 (one message or a batch) and answers with JSON. Credentials are an API key or
//! an OAuth access token in `Authorization: Bearer`; without them the answer points to the OAuth metadata, so apps
//! such as ChatGPT and Claude can sign in on their own.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use celsus_core::Language;
use celsus_core::assist::Submitted;
use serde_json::{Value, json};

use crate::auth::Auth;
use crate::routes::books::{add_book, visible, visible_books};
use crate::web::{ApiError, ApiResult};
use crate::{AppState, assist, audit, library};

const PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Celsus is a book translation library on a server. To translate a book: find it with \
list_books (or add a PDF with add_book), then repeat get_text_to_translate and submit_translations until no blocks \
are left. Translate the blocks yourself; Celsus checks each translation, returns the ones it rejects with the reason, \
and writes finished pages into the translated PDF. get_output gives its address.

Covers and other pictures with text in them are redrawn, not typed over. To translate a cover: list_pictures finds \
it, get_picture gives you the image. If you can generate images, draw it yourself: the same picture with every text \
translated, the artwork, colours and lettering style unchanged, the same proportions, and send it as base64 with \
replace_picture. Only if you cannot generate images, use redraw_picture, which asks the picture model chosen on the \
server to draw it. restore_picture brings the original back.";

/// Pictures larger than this are not sent back; models take only a few megabytes per image.
const MAX_PICTURE: usize = 8 << 20;

fn tools() -> Value {
    let book = json!({ "type": "string", "description": "Book id from list_books." });
    let lang = json!({ "type": "string", "description": "Language code of any major world language, e.g. tr, de, ar, hi, ja, zh-hans, pt-br." });
    let page = json!({ "type": "integer", "description": "Default 1 (the cover)." });
    let picture = json!({ "type": "string", "description": "From list_pictures; default the largest picture on the page." });
    let read = json!({ "readOnlyHint": true });
    json!([
        { "name": "list_books", "annotations": read,
          "description": "Books in the library you can see, with page counts and how many pages are translated into each language.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "add_book",
          "description": "Add a PDF to the library, sent as base64. It is private to you unless shared.",
          "inputSchema": { "type": "object", "properties": {
              "pdf_base64": { "type": "string" },
              "file_name": { "type": "string", "description": "e.g. Book.pdf" },
              "shared": { "type": "boolean", "description": "Let every member of the library see it." } },
            "required": ["pdf_base64"] } },
        { "name": "book_info", "annotations": read,
          "description": "Details of one book; with a language, which pages are already translated into it.",
          "inputSchema": { "type": "object", "properties": { "book_id": book, "target_language": lang }, "required": ["book_id"] } },
        { "name": "get_text_to_translate", "annotations": read,
          "description": "The next blocks of text to translate from the unfinished pages, with instructions and surrounding context. Call again after submitting; an empty block list means the requested pages are done.",
          "inputSchema": { "type": "object", "properties": {
              "book_id": book, "target_language": lang,
              "pages": { "type": "string", "description": "Pages to work on, e.g. \"10-12,15\". Omit for the whole book." },
              "max_characters": { "type": "integer", "description": "About how much text to return (default 4000)." } },
            "required": ["book_id", "target_language"] } },
        { "name": "submit_translations",
          "description": "Send translations back. Each item needs the page and id from get_text_to_translate. Translations that fail Celsus's checks are returned with the problem; fix and send them again, or set accept_anyway if the source really calls for it.",
          "inputSchema": { "type": "object", "properties": {
              "book_id": book, "target_language": lang,
              "translations": { "type": "array", "items": { "type": "object", "properties": {
                  "page": { "type": "integer" }, "id": { "type": "string" }, "text": { "type": "string" } },
                  "required": ["page", "id", "text"] } },
              "accept_anyway": { "type": "boolean" } },
            "required": ["book_id", "target_language", "translations"] } },
        { "name": "get_output", "annotations": read,
          "description": "Where the translated PDF of a book can be downloaded (with the same credentials), and how many pages it has translated.",
          "inputSchema": { "type": "object", "properties": { "book_id": book, "target_language": lang }, "required": ["book_id", "target_language"] } },
        { "name": "list_pictures", "annotations": read,
          "description": "Pictures in a book (page 1 is usually the cover), largest first on each page, with whether each is already redrawn in the language.",
          "inputSchema": { "type": "object", "properties": {
              "book_id": book, "target_language": lang,
              "pages": { "type": "string", "description": "e.g. \"1\" or \"1-20\". Default \"1\". \"all\" for the whole book." } },
            "required": ["book_id"] } },
        { "name": "get_picture", "annotations": read,
          "description": "Look at a picture: returns the image itself. With target_language and redrawn=true, the redrawn version.",
          "inputSchema": { "type": "object", "properties": {
              "book_id": book, "page": page, "picture_id": picture, "target_language": lang, "redrawn": { "type": "boolean" } },
            "required": ["book_id"] } },
        { "name": "replace_picture",
          "description": "The way to translate a cover or picture: look at it with get_picture, draw it yourself with every text translated (same artwork, colours, lettering style and proportions), and send it here as base64 PNG or JPEG. Celsus puts it into the translated PDF in place of the original.",
          "inputSchema": { "type": "object", "properties": {
              "book_id": book, "target_language": lang, "page": page, "picture_id": picture,
              "image_base64": { "type": "string", "description": "PNG or JPEG file as base64 (a data: URL is fine)." } },
            "required": ["book_id", "target_language", "image_base64"] } },
        { "name": "redraw_picture",
          "description": "Only for assistants that cannot generate images: the server asks its picture model to redraw a picture (by default the cover) with its text translated. If you can generate images, draw the picture yourself and use replace_picture instead.",
          "inputSchema": { "type": "object", "properties": {
              "book_id": book, "target_language": lang, "page": page, "picture_id": picture,
              "note": { "type": "string", "description": "Extra wishes, e.g. the exact translated title to use." } },
            "required": ["book_id", "target_language"] } },
        { "name": "restore_picture",
          "description": "Put the original picture back in the translated PDF.",
          "inputSchema": { "type": "object", "properties": { "book_id": book, "target_language": lang, "page": page, "picture_id": picture },
            "required": ["book_id", "target_language"] } },
    ])
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

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn arg_lang(args: &Value) -> ApiResult<Language> {
    let code = arg_str(args, "target_language")
        .ok_or_else(|| ApiError::bad("target_language is required"))?;
    let lang: Language = code
        .parse()
        .map_err(|e: celsus_core::language::InvalidLanguage| ApiError::bad(e.to_string()))?;
    if !library::valid_lang(&assist::code(&lang)) {
        return Err(ApiError::bad("Unknown language code."));
    }
    Ok(lang)
}

fn arg_page(args: &Value) -> ApiResult<usize> {
    match &args["page"] {
        Value::Null => Ok(1),
        v => v
            .as_u64()
            .filter(|&n| n >= 1)
            .map(|n| n as usize)
            .ok_or_else(|| ApiError::bad("page must be a page number")),
    }
}

/// Parse "10-12,15" (or a list of numbers) into page numbers.
fn parse_pages(v: Option<&Value>) -> ApiResult<Vec<usize>> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_u64()
                    .map(|n| n as usize)
                    .ok_or_else(|| ApiError::bad("pages must be numbers"))
            })
            .collect(),
        Some(Value::Number(n)) => Ok(vec![n.as_u64().unwrap_or(0) as usize]),
        Some(Value::String(s)) => {
            let mut out = Vec::new();
            for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let num = |x: &str| {
                    x.trim()
                        .parse::<usize>()
                        .map_err(|_| ApiError::bad(format!("bad page {x}")))
                };
                match part.split_once('-') {
                    Some((a, b)) => {
                        let (a, b) = (num(a)?, num(b)?);
                        if b < a || b - a > 5000 {
                            return Err(ApiError::bad(format!("bad range {part}")));
                        }
                        out.extend(a..=b);
                    }
                    None => out.push(num(part)?),
                }
            }
            Ok(out)
        }
        _ => Err(ApiError::bad("pages must be text like \"10-12,15\"")),
    }
}

fn base64_arg(args: &Value, key: &str) -> ApiResult<Vec<u8>> {
    let raw = arg_str(args, key).ok_or_else(|| ApiError::bad(format!("{key} is required")))?;
    let b64 = raw.split_once("base64,").map_or(raw, |(_, d)| d);
    celsus_core::base64::decode(b64)
        .ok_or_else(|| ApiError::bad(format!("{key} is not valid base64")))
}

fn absolute(state: &AppState, path: &str) -> String {
    match &state.config.public_origin {
        Some(origin) => format!("{origin}{path}"),
        None => path.to_owned(),
    }
}

async fn call_tool(state: &AppState, auth: &Auth, name: &str, args: &Value) -> ApiResult<Output> {
    let book_arg = || arg_str(args, "book_id").ok_or_else(|| ApiError::bad("book_id is required"));
    Ok(match name {
        "list_books" => {
            auth.require("read")?;
            let books = visible_books(state, auth).await?;
            json!({ "books": books.iter().map(|b| json!({
                "book_id": b["id"], "title": b["title"], "author": b["author"], "pages": b["pages"],
                "translated_pages": b["translated"], "busy": b["busy"], "shared": b["visibility"] == "shared" })).collect::<Vec<_>>() })
            .into()
        }
        "add_book" => {
            auth.require("translate")?;
            let pdf = base64_arg(args, "pdf_base64")?;
            if pdf.len() > state.config.max_upload {
                return Err(ApiError::bad("The PDF is larger than this server accepts."));
            }
            let name = arg_str(args, "file_name").map_or_else(
                || "book.pdf".to_owned(),
                |n| library::clean(&n.replace(['/', '\\'], "_"), 200),
            );
            let visibility = if args["shared"].as_bool().unwrap_or(false) {
                "shared"
            } else {
                "private"
            };
            let b = add_book(state, auth, Bytes::from(pdf), name, visibility.to_owned()).await?;
            json!({ "book_id": b["id"], "title": b["title"], "author": b["author"], "pages": b["pages"] }).into()
        }
        "book_info" => {
            auth.require("read")?;
            let db = state.pool.get().await?;
            let book = visible(&**db, auth, book_arg()?).await?;
            let row = db
                .query_one(
                    "SELECT title, author, subject, source_lang, pages FROM books WHERE id = $1",
                    &[&book.id],
                )
                .await?;
            let mut out = json!({ "book_id": book.id, "title": row.get::<_, String>(0), "author": row.get::<_, String>(1),
                "subject": row.get::<_, String>(2), "source_language": row.get::<_, String>(3), "pages": row.get::<_, i32>(4) });
            if let Ok(lang) = arg_lang(args) {
                let rows = db
                    .query("SELECT page, kept FROM translated_pages WHERE book_id = $1 AND lang = $2 ORDER BY page", &[&book.id, &assist::code(&lang)])
                    .await?;
                out["done_pages"] =
                    json!(rows.iter().map(|r| r.get::<_, i32>(0)).collect::<Vec<_>>());
                out["pages_with_blocks_kept_original"] = json!(
                    rows.iter()
                        .filter(|r| r.get::<_, i32>(1) > 0)
                        .map(|r| r.get::<_, i32>(0))
                        .collect::<Vec<_>>()
                );
            }
            out.into()
        }
        "get_text_to_translate" => {
            auth.require("read")?;
            let lang = arg_lang(args)?;
            let db = state.pool.get().await?;
            let book = visible(&**db, auth, book_arg()?).await?;
            drop(db);
            let max = args["max_characters"]
                .as_u64()
                .unwrap_or(4000)
                .clamp(500, 20000) as usize;
            let p = assist::pending(state, &book.id, &lang, parse_pages(args.get("pages"))?, max)
                .await?;
            let mut v = serde_json::to_value(p).map_err(ApiError::internal)?;
            v["book_id"] = json!(book.id);
            v.into()
        }
        "submit_translations" => {
            auth.require("translate")?;
            let lang = arg_lang(args)?;
            let db = state.pool.get().await?;
            let book = visible(&**db, auth, book_arg()?).await?;
            drop(db);
            let items: Vec<Submitted> = serde_json::from_value(args["translations"].clone())
                .map_err(|e| ApiError::bad(format!("translations: {e}")))?;
            if items.len() > 2000 {
                return Err(ApiError::bad("Send at most 2000 translations at a time."));
            }
            let mut r = assist::submit(
                state,
                &book.id,
                &lang,
                items,
                args["accept_anyway"].as_bool().unwrap_or(false),
            )
            .await?;
            r.output = r.output.map(|p| absolute(state, &p));
            serde_json::to_value(r).map_err(ApiError::internal)?.into()
        }
        "get_output" => {
            auth.require("read")?;
            let lang = arg_lang(args)?;
            let db = state.pool.get().await?;
            let book = visible(&**db, auth, book_arg()?).await?;
            let pages: i64 = db
                .query_one(
                    "SELECT COUNT(*) FROM translated_pages WHERE book_id = $1 AND lang = $2",
                    &[&book.id, &assist::code(&lang)],
                )
                .await?
                .get(0);
            let exists =
                library::output_file(&state.config, &book.id, &assist::code(&lang)).exists();
            json!({ "url": exists.then(|| absolute(state, &format!("/api/books/{}/file/{}", book.id, assist::code(&lang)))),
                    "translated_pages": pages }).into()
        }
        "list_pictures" => {
            auth.require("read")?;
            let db = state.pool.get().await?;
            let book = visible(&**db, auth, book_arg()?).await?;
            drop(db);
            let lang = arg_lang(args).ok().map(|l| assist::code(&l));
            let pages = match args.get("pages") {
                Some(Value::String(s)) if s.trim().eq_ignore_ascii_case("all") => Vec::new(),
                None | Some(Value::Null) => vec![1],
                v => parse_pages(v)?,
            };
            let list = assist::pictures(state, &book.id, lang.as_deref(), pages).await?;
            json!({ "pictures": list.iter().map(|p| json!({ "picture_id": p.picture.image, "page": p.picture.page,
                "width": p.picture.width, "height": p.picture.height, "redrawn": p.redrawn })).collect::<Vec<_>>() }).into()
        }
        "get_picture" => {
            auth.require("read")?;
            let db = state.pool.get().await?;
            let book = visible(&**db, auth, book_arg()?).await?;
            drop(db);
            let page = arg_page(args)?;
            let image = arg_str(args, "picture_id").map(str::to_owned);
            let (id, bytes, mime) = if args["redrawn"].as_bool().unwrap_or(false) {
                let lang = arg_lang(args)?;
                assist::redrawn(state, &book.id, &assist::code(&lang), page, image).await?
            } else {
                assist::original(state, &book.id, page, image).await?
            };
            if bytes.len() > MAX_PICTURE {
                return Err(ApiError::bad(format!(
                    "The picture is {} MB, too large to send.",
                    bytes.len() >> 20
                )));
            }
            Output {
                value: json!({ "picture_id": id, "page": page, "bytes": bytes.len(), "type": mime }),
                images: vec![(bytes, mime)],
            }
        }
        "replace_picture" | "redraw_picture" | "restore_picture" => {
            auth.require("translate")?;
            let lang = arg_lang(args)?;
            let db = state.pool.get().await?;
            let book = visible(&**db, auth, book_arg()?).await?;
            let page = arg_page(args)?;
            let image = arg_str(args, "picture_id").map(str::to_owned);
            let picture = match name {
                "replace_picture" => {
                    assist::place(
                        state,
                        &book.id,
                        &lang,
                        page,
                        image,
                        base64_arg(args, "image_base64")?,
                    )
                    .await?
                }
                "redraw_picture" => {
                    assist::redraw(
                        state,
                        &book.id,
                        &lang,
                        page,
                        image,
                        arg_str(args, "note").map(str::to_owned),
                    )
                    .await?
                }
                _ => {
                    assist::restore(state, &book.id, &lang, page, image).await?;
                    String::new()
                }
            };
            audit::record(
                &**db,
                Some(auth.user.id),
                &auth.client,
                "picture_changed",
                Some(&book.id),
                json!({ "tool": name, "lang": assist::code(&lang), "page": page }),
            )
            .await?;
            json!({ "page": page, "picture_id": (!picture.is_empty()).then_some(picture),
                    "output": absolute(state, &format!("/api/books/{}/file/{}", book.id, assist::code(&lang))),
                    "next": "Look at it with get_picture (redrawn=true)." }).into()
        }
        _ => return Err(ApiError::bad(format!("unknown tool {name}"))),
    })
}

async fn handle(state: &AppState, auth: &Auth, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "celsus", "title": "Celsus", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            Ok(match call_tool(state, auth, name, &args).await {
                Ok(Output { value, images }) => {
                    let mut content = vec![json!({ "type": "text", "text": value.to_string() })];
                    content.extend(images.into_iter().map(|(bytes, mime)| {
                        json!({ "type": "image", "data": celsus_core::base64::encode(&bytes), "mimeType": mime })
                    }));
                    let structured = if value.is_object() {
                        value
                    } else {
                        json!({ "result": value })
                    };
                    json!({ "content": content, "structuredContent": structured, "isError": false })
                }
                Err(e) => {
                    json!({ "content": [{ "type": "text", "text": e.message }], "isError": true })
                }
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

/// Ask for credentials in the way MCP clients understand: where to find the OAuth server.
pub fn unauthorized(state: &AppState) -> Response {
    let metadata = absolute(state, "/.well-known/oauth-protected-resource");
    let mut res = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized", "message": "Sign in first: an API key or an OAuth access token." })),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer resource_metadata=\"{metadata}\"")) {
        res.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    res
}

pub async fn post(
    State(state): State<AppState>,
    auth: Result<Auth, ApiError>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Only bearer credentials: a browser's session cookie is not an MCP client.
    let Ok(auth) = auth
        .ok()
        .filter(|_| headers.contains_key(header::AUTHORIZATION))
        .ok_or(())
    else {
        return unauthorized(&state);
    };
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }))
                .into_response();
        }
    };
    let reply = match msg {
        Value::Array(batch) => {
            let mut out = Vec::new();
            for m in batch.iter().take(50) {
                if let Some(r) = handle(&state, &auth, m).await {
                    out.push(r);
                }
            }
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => handle(&state, &auth, &m).await,
    };
    match reply {
        Some(r) => Json(r).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// This server answers each request directly and opens no event stream.
pub async fn other() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}
