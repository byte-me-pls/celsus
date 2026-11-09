//! Covers through MCP: listing and looking at pictures, redrawing them with the picture model chosen in Celsus
//! (a stand-in server here), placing a picture the assistant made, and going back to the original.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use celsus_mcp::handle;
use serde_json::{Value, json};

/// A one-page PDF with a 64x64 grey picture drawn over most of the page.
fn cover_pdf() -> Vec<u8> {
    let pixels = celsus_pdf::filters::deflate(&vec![200u8; 64 * 64]);
    let content = b"q 400 0 0 600 100 100 cm /Im1 Do Q";
    let stream = |dict: String, data: &[u8]| {
        let mut o = format!("{dict}\nstream\n").into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        o
    };
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        stream(
            format!(
                "<< /Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                pixels.len()
            ),
            &pixels,
        ),
        stream(format!("<< /Length {} >>", content.len()), content),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(o);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// A stand-in picture model: answers each request with `picture`, and keeps the request bodies.
/// Requests the stand-in server received: path and JSON body.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

fn picture_server(picture: Vec<u8>, answers: usize) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for _ in 0..answers {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
            let mut length = 0;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            log.lock()
                .unwrap()
                .push((path, serde_json::from_slice(&body).unwrap_or_default()));
            let answer = json!({ "data": [{ "b64_json": celsus_core::base64::encode(&picture) }] })
                .to_string();
            let mut out = stream;
            write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            )
            .unwrap();
        }
    });
    (url, seen)
}

fn call(name: &str, args: Value) -> Value {
    let reply = handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                "params": { "name": name, "arguments": args } }))
    .unwrap();
    reply["result"].clone()
}

fn ok(name: &str, args: Value) -> Value {
    let r = call(name, args);
    assert_eq!(r["isError"], false, "{name}: {}", r["content"][0]["text"]);
    r
}

fn error(name: &str, args: Value) -> String {
    let r = call(name, args);
    assert_eq!(r["isError"], true, "{name} should fail: {r}");
    r["content"][0]["text"].as_str().unwrap().to_owned()
}

#[test]
fn covers_are_listed_shown_redrawn_replaced_and_restored() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mcp-pictures-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Only this test runs in this process, so the data folder can be set for all of it.
    unsafe { std::env::set_var("CELSUS_DATA_DIR", &dir) };

    let tools = handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let names: Vec<String> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    // Looking needs no approval in clients that honour the hint; changing the book does.
    let read_only = |name: &str| {
        tools.iter().find(|t| t["name"] == name).unwrap()["annotations"]["readOnlyHint"] == true
    };
    assert!(read_only("list_pictures") && read_only("get_picture") && read_only("list_books"));
    assert!(
        !read_only("replace_picture")
            && !read_only("redraw_picture")
            && !read_only("submit_translations")
    );
    for tool in [
        "list_pictures",
        "get_picture",
        "redraw_picture",
        "replace_picture",
        "restore_picture",
    ] {
        assert!(names.iter().any(|n| n == tool), "{tool} missing");
    }

    let pdf = dir.join("Cover Book.pdf");
    std::fs::write(&pdf, cover_pdf()).unwrap();
    let book = ok("add_book", json!({ "path": pdf.display().to_string() }))["structuredContent"]["book_id"]
        .as_i64()
        .unwrap();

    // The cover is found, and without a picture model the assistant is told what to do.
    let listed = ok(
        "list_pictures",
        json!({ "book_id": book, "target_language": "tr" }),
    )["structuredContent"]
        .clone();
    let pictures = listed["pictures"].as_array().unwrap();
    assert_eq!(pictures.len(), 1, "{listed}");
    let id = pictures[0]["picture_id"].as_str().unwrap().to_owned();
    assert_eq!(
        (
            pictures[0]["width"].as_u64(),
            pictures[0]["redrawn"].as_bool()
        ),
        (Some(64), Some(false))
    );
    assert!(listed["picture_model"].is_null());
    assert!(listed["note"].as_str().unwrap().contains("replace_picture"));

    // The assistant can look at it.
    let shown = ok("get_picture", json!({ "book_id": book }));
    assert_eq!(shown["content"][1]["type"], "image");
    assert_eq!(shown["content"][1]["mimeType"], "image/png");
    let original =
        celsus_core::base64::decode(shown["content"][1]["data"].as_str().unwrap()).unwrap();
    assert!(original.starts_with(b"\x89PNG"));

    let no_model = error(
        "redraw_picture",
        json!({ "book_id": book, "target_language": "tr" }),
    );
    assert!(no_model.contains("Settings → Models"), "{no_model}");

    // Choose a picture model (a stand-in server). The connection gets an id no real one has, so no stored key is read.
    let drawn = celsus_pdf::image::png(96, 144, 3, &vec![30u8; 96 * 144 * 3]);
    let (url, seen) = picture_server(drawn.clone(), 1);
    {
        let conn = celsus_store::open().unwrap();
        let p = celsus_store::db::Provider {
            id: 0,
            name: "Stand-in".into(),
            url,
            model: "gpt-image-1".into(),
            is_default: false,
            has_key: false,
        };
        let pid = celsus_store::db::save_provider(&conn, &p).unwrap();
        conn.execute("UPDATE providers SET id = 987654 WHERE id = ?", [pid])
            .unwrap();
        celsus_store::db::set_setting(&conn, "model.image", "987654:gpt-image-1").unwrap();
    }
    let listed = ok("list_pictures", json!({ "book_id": book }))["structuredContent"].clone();
    assert_eq!(listed["picture_model"], "Stand-in / gpt-image-1");

    let done = ok(
        "redraw_picture",
        json!({ "book_id": book, "target_language": "tr", "note": "Title: Kapak Kitabı" }),
    )["structuredContent"]
        .clone();
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "/v1/images/edits");
    let prompt = requests[0].1["prompt"].as_str().unwrap();
    assert!(
        prompt.contains("the cover of")
            && prompt.contains("Turkish")
            && prompt.contains("Kapak Kitabı"),
        "{prompt}"
    );
    assert!(
        requests[0].1["images"][0]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(
        std::fs::read(done["redrawn_file"].as_str().unwrap()).unwrap(),
        drawn
    );
    let output = std::path::PathBuf::from(done["output"].as_str().unwrap());
    assert!(output.exists());
    // The translated PDF carries the new picture.
    let doc = celsus_pdf::Document::open(&output).unwrap();
    let page = &doc.pages().unwrap()[0];
    let placed = celsus_pdf::image::page_images(&doc, page);
    assert_eq!((placed[0].width, placed[0].height), (96, 144));

    let shown = ok(
        "get_picture",
        json!({ "book_id": book, "target_language": "tr", "redrawn": true }),
    );
    assert_eq!(
        celsus_core::base64::decode(shown["content"][1]["data"].as_str().unwrap()).unwrap(),
        drawn
    );

    // A picture the assistant made itself, as base64 or a file.
    let own = celsus_pdf::image::png(80, 120, 3, &vec![90u8; 80 * 120 * 3]);
    ok(
        "replace_picture",
        json!({ "book_id": book, "target_language": "tr", "picture_id": id,
                "image_base64": format!("data:image/png;base64,{}", celsus_core::base64::encode(&own)) }),
    );
    let doc = celsus_pdf::Document::open(&output).unwrap();
    let placed = celsus_pdf::image::page_images(&doc, &doc.pages().unwrap()[0]);
    assert_eq!((placed[0].width, placed[0].height), (80, 120));
    let file = dir.join("own.png");
    std::fs::write(&file, &own).unwrap();
    ok(
        "replace_picture",
        json!({ "book_id": book, "target_language": "tr", "file_path": file.display().to_string() }),
    );
    let bad = error(
        "replace_picture",
        json!({ "book_id": book, "target_language": "tr", "image_base64": "aGVsbG8=" }),
    );
    assert!(bad.contains("JPEG and PNG"), "{bad}");
    let wrong_page = error(
        "replace_picture",
        json!({ "book_id": book, "target_language": "tr", "page": 7, "file_path": file.display().to_string() }),
    );
    assert!(wrong_page.contains("no page 7"), "{wrong_page}");

    // Back to the original.
    ok(
        "restore_picture",
        json!({ "book_id": book, "target_language": "tr" }),
    );
    let listed = ok(
        "list_pictures",
        json!({ "book_id": book, "target_language": "tr" }),
    )["structuredContent"]
        .clone();
    assert_eq!(listed["pictures"][0]["redrawn"], false);
    let doc = celsus_pdf::Document::open(&output).unwrap();
    let placed = celsus_pdf::image::page_images(&doc, &doc.pages().unwrap()[0]);
    assert_eq!((placed[0].width, placed[0].height), (64, 64));

    let _ = std::fs::remove_dir_all(&dir);
}
