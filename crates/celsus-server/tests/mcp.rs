//! MCP on the server: credentials, translating a book block by block, covers drawn by the assistant, OAuth for
//! apps such as ChatGPT and Claude, and the web interface's picture, estimate, usage and settings endpoints.
//!
//! Needs `CELSUS_TEST_DATABASE_URL` (see `common`).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{Value, json};

/// A one-page PDF with a line of text and a 120x160 picture drawn over most of the page.
fn cover_pdf() -> Vec<u8> {
    let pixels = celsus_pdf::filters::deflate(&vec![180u8; 120 * 160]);
    let content =
        b"q 400 0 0 520 100 200 cm /Im1 Do Q BT /F1 18 Tf 72 100 Td (A Book About Libraries) Tj ET";
    let stream = |dict: String, data: &[u8]| {
        let mut o = format!("{dict}\nstream\n").into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        o
    };
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 4 0 R >> /Font << /F1 6 0 R >> >> /Contents 5 0 R >>".to_vec(),
        stream(
            format!("<< /Type /XObject /Subtype /Image /Width 120 /Height 160 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>", pixels.len()),
            &pixels,
        ),
        stream(format!("<< /Length {} >>", content.len()), content),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
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

async fn upload(s: &Server, b: &Browser, pdf: Vec<u8>) -> String {
    let r = s
        .send_raw(
            b,
            Method::POST,
            "/api/books",
            pdf,
            &[
                ("x-filename", "Book.pdf"),
                ("content-type", "application/pdf"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    r.body["book"]["id"].as_str().unwrap().to_owned()
}

async fn key(s: &Server, b: &Browser, scopes: &[&str]) -> String {
    let r = s
        .post(b, "/api/keys", json!({ "name": "chat", "scopes": scopes }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    r.body["key"].as_str().unwrap().to_owned()
}

/// One JSON-RPC message to `/mcp` with a bearer credential.
async fn rpc(s: &Server, token: &str, msg: Value) -> Reply {
    s.send_raw(
        &Browser::default(),
        Method::POST,
        "/mcp",
        msg.to_string().into_bytes(),
        &[
            ("authorization", &format!("Bearer {token}")),
            ("content-type", "application/json"),
        ],
    )
    .await
}

/// Call a tool; returns the result (content, structuredContent, isError).
async fn tool(s: &Server, token: &str, name: &str, args: Value) -> Value {
    let r = rpc(s, token, json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": args } })).await;
    assert_eq!(r.status, StatusCode::OK, "{name}: {:?}", r.body);
    r.body["result"].clone()
}

async fn ok(s: &Server, token: &str, name: &str, args: Value) -> Value {
    let r = tool(s, token, name, args).await;
    assert_eq!(r["isError"], false, "{name}: {}", r["content"][0]["text"]);
    r["structuredContent"].clone()
}

async fn fails(s: &Server, token: &str, name: &str, args: Value) -> String {
    let r = tool(s, token, name, args).await;
    assert_eq!(r["isError"], true, "{name} should fail: {r}");
    r["content"][0]["text"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn mcp_asks_for_credentials_and_points_to_oauth() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let anon = Browser::default();
    let r = s
        .send_raw(
            &anon,
            Method::POST,
            "/mcp",
            b"{}".to_vec(),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let www = r.headers.get("www-authenticate").unwrap().to_str().unwrap();
    assert!(
        www.contains("resource_metadata=\"https://lib.test/.well-known/oauth-protected-resource\""),
        "{www}"
    );
    // A browser session is not an MCP credential.
    let r = s
        .send_raw(
            &owner,
            Method::POST,
            "/mcp",
            b"{}".to_vec(),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = rpc(
        &s,
        "celsus_0123456789abcdef_not-a-real-secret-at-all-000000000",
        json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Discovery.
    let r = s.get(&anon, "/.well-known/oauth-protected-resource").await;
    assert_eq!(r.body["resource"], "https://lib.test/mcp");
    assert_eq!(r.body["authorization_servers"][0], "https://lib.test");
    let r = s
        .get(&anon, "/.well-known/oauth-authorization-server")
        .await;
    assert_eq!(r.body["token_endpoint"], "https://lib.test/oauth/token");
    assert_eq!(r.body["code_challenge_methods_supported"], json!(["S256"]));
    // No event stream.
    assert_eq!(
        s.get(&anon, "/mcp").await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    // Another site's page cannot call it with someone's key.
    let k = key(&s, &owner, &["read"]).await;
    let r = s
        .send_raw(
            &anon,
            Method::POST,
            "/mcp",
            b"{}".to_vec(),
            &[
                ("authorization", &format!("Bearer {k}")),
                ("origin", "https://evil.example"),
                ("content-type", "application/json"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    s.finish().await;
}

#[tokio::test]
async fn a_book_is_translated_over_mcp_with_an_api_key() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let book = upload(&s, &owner, sample_pdf("Chapter 7 begins here")).await;
    let k = key(&s, &owner, &["read", "translate"]).await;
    let reader = key(&s, &owner, &["read"]).await;

    let r = rpc(&s, &k, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).await;
    assert_eq!(r.body["result"]["serverInfo"]["name"], "celsus");
    assert!(
        r.body["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("replace_picture")
    );
    // A notification gets no answer.
    let r = rpc(
        &s,
        &k,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let r = rpc(
        &s,
        &k,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    let tools: Vec<&str> = r.body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in [
        "list_books",
        "add_book",
        "get_text_to_translate",
        "submit_translations",
        "get_output",
        "list_pictures",
        "get_picture",
        "replace_picture",
        "redraw_picture",
        "restore_picture",
    ] {
        assert!(tools.contains(&t), "{t} missing");
    }
    // A batch.
    let r = rpc(&s, &k, json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }, { "jsonrpc": "2.0", "id": 2, "method": "ping" }])).await;
    assert_eq!(r.body.as_array().unwrap().len(), 2);

    let books = ok(&s, &k, "list_books", json!({})).await;
    assert_eq!(books["books"][0]["book_id"], book.as_str());
    let todo = ok(
        &s,
        &k,
        "get_text_to_translate",
        json!({ "book_id": book, "target_language": "tr" }),
    )
    .await;
    let blocks = todo["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 1, "{todo}");
    assert_eq!(blocks[0]["text"], "Chapter 7 begins here");
    let item = json!([{ "page": blocks[0]["page"], "id": blocks[0]["id"], "text": "Bölüm 7 burada başlıyor" }]);
    // A key that may only read cannot write.
    let refused = fails(
        &s,
        &reader,
        "submit_translations",
        json!({ "book_id": book, "target_language": "tr", "translations": item }),
    )
    .await;
    assert!(refused.contains("not allowed"), "{refused}");
    // A check that fails (the number is gone) is returned with the reason.
    let bad = ok(
        &s,
        &k,
        "submit_translations",
        json!({ "book_id": book, "target_language": "tr",
        "translations": [{ "page": 1, "id": blocks[0]["id"], "text": "Bölüm burada başlıyor" }] }),
    )
    .await;
    assert_eq!(bad["accepted"], 0);
    assert!(
        !bad["rejected"][0]["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let done = ok(
        &s,
        &k,
        "submit_translations",
        json!({ "book_id": book, "target_language": "tr", "translations": item }),
    )
    .await;
    assert_eq!(done["accepted"], 1);
    assert_eq!(done["completed_pages"], json!([1]));
    assert_eq!(
        done["output"],
        format!("https://lib.test/api/books/{book}/file/tr")
    );
    let left = ok(
        &s,
        &k,
        "get_text_to_translate",
        json!({ "book_id": book, "target_language": "tr" }),
    )
    .await;
    assert_eq!(left["blocks"], json!([]));
    let out = ok(
        &s,
        &k,
        "get_output",
        json!({ "book_id": book, "target_language": "tr" }),
    )
    .await;
    assert_eq!(out["translated_pages"], 1);
    // The translated PDF is there, for the web interface and the key alike.
    let pdf = s
        .send_raw(
            &Browser::default(),
            Method::GET,
            &format!("/api/books/{book}/file/tr"),
            vec![],
            &[("authorization", &format!("Bearer {k}"))],
        )
        .await;
    assert_eq!(pdf.status, StatusCode::OK);
    let doc = celsus_pdf::Document::load(pdf.bytes).unwrap();
    let page = &doc.pages().unwrap()[0];
    let text: String = celsus_core::extract::extract_page(&doc, page, 1)
        .unwrap()
        .layout
        .blocks
        .iter()
        .map(|b| b.plain())
        .collect();
    assert!(text.contains("Bölüm 7"), "{text}");
    let pages = s.get(&owner, &format!("/api/books/{book}/pages/tr")).await;
    assert_eq!(pages.body["pages"], json!([[1, 0]]));
    // Languages written in different case are the same language.
    let info = ok(
        &s,
        &k,
        "book_info",
        json!({ "book_id": book, "target_language": "TR" }),
    )
    .await;
    assert_eq!(info["done_pages"], json!([1]));

    // Someone else's private book does not exist for this key.
    let member = s.member(&owner, "other@lib.test", "member").await;
    let theirs = upload(&s, &member, sample_pdf("Private")).await;
    let missing = fails(
        &s,
        &k,
        "get_text_to_translate",
        json!({ "book_id": theirs, "target_language": "tr" }),
    )
    .await;
    assert!(missing.contains("Not found"), "{missing}");
    // A book sent over MCP.
    let added = ok(&s, &k, "add_book", json!({ "pdf_base64": celsus_core::base64::encode(&sample_pdf("Sent over MCP")), "file_name": "sent.pdf" })).await;
    assert_eq!(added["pages"], 1);
    s.finish().await;
}

#[tokio::test]
async fn covers_are_drawn_by_the_assistant_and_placed() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let book = upload(&s, &owner, cover_pdf()).await;
    let k = key(&s, &owner, &["read", "translate"]).await;

    let list = ok(
        &s,
        &k,
        "list_pictures",
        json!({ "book_id": book, "target_language": "tr" }),
    )
    .await;
    let pic = &list["pictures"][0];
    assert_eq!(
        (pic["width"].as_u64(), pic["redrawn"].as_bool()),
        (Some(120), Some(false))
    );
    let shown = tool(&s, &k, "get_picture", json!({ "book_id": book })).await;
    assert_eq!(shown["content"][1]["type"], "image");
    assert_eq!(shown["content"][1]["mimeType"], "image/png");

    // Without a picture model on the server, the assistant is told to draw it itself.
    let no_model = fails(
        &s,
        &k,
        "redraw_picture",
        json!({ "book_id": book, "target_language": "tr" }),
    )
    .await;
    assert!(no_model.contains("No picture model"), "{no_model}");

    let drawn = celsus_pdf::image::png(90, 120, 3, &vec![40u8; 90 * 120 * 3]);
    let placed = ok(
        &s,
        &k,
        "replace_picture",
        json!({ "book_id": book, "target_language": "tr",
        "image_base64": format!("data:image/png;base64,{}", celsus_core::base64::encode(&drawn)) }),
    )
    .await;
    assert_eq!(placed["picture_id"], pic["picture_id"]);
    let bad = fails(
        &s,
        &k,
        "replace_picture",
        json!({ "book_id": book, "target_language": "tr", "image_base64": "aGVsbG8=" }),
    )
    .await;
    assert!(bad.contains("JPEG and PNG"), "{bad}");

    // The translated PDF carries the new cover; the web interface sees it too.
    let pdf = s.get(&owner, &format!("/api/books/{book}/file/tr")).await;
    let doc = celsus_pdf::Document::load(pdf.bytes).unwrap();
    let images = celsus_pdf::image::page_images(&doc, &doc.pages().unwrap()[0]);
    assert_eq!((images[0].width, images[0].height), (90, 120));
    let web = s
        .get(
            &owner,
            &format!("/api/books/{book}/pictures?page=1&lang=tr"),
        )
        .await;
    assert_eq!(web.body["pictures"][0]["redrawn"], true, "{:?}", web.body);
    let again = tool(
        &s,
        &k,
        "get_picture",
        json!({ "book_id": book, "target_language": "tr", "redrawn": true }),
    )
    .await;
    assert_eq!(
        celsus_core::base64::decode(again["content"][1]["data"].as_str().unwrap()).unwrap(),
        drawn
    );
    let original = s
        .get(
            &owner,
            &format!("/api/books/{book}/pictures/original?page=1"),
        )
        .await;
    assert_eq!(original.status, StatusCode::OK);
    assert!(original.bytes.starts_with(b"\x89PNG"));

    // The web interface can upload a picture and go back to the original.
    let own = celsus_pdf::image::png(60, 80, 3, &vec![200u8; 60 * 80 * 3]);
    let r = s
        .send_raw(
            &owner,
            Method::PUT,
            &format!("/api/books/{book}/pictures/own?page=1&lang=tr"),
            own.clone(),
            &[("content-type", "image/png")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = s
        .get(
            &owner,
            &format!("/api/books/{book}/pictures/redrawn?page=1&lang=tr"),
        )
        .await;
    assert_eq!(r.bytes, own);
    let r = s
        .send(
            &owner,
            Method::DELETE,
            &format!("/api/books/{book}/pictures/own?page=1&lang=tr"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let after = ok(
        &s,
        &k,
        "list_pictures",
        json!({ "book_id": book, "target_language": "tr" }),
    )
    .await;
    assert_eq!(after["pictures"][0]["redrawn"], false);
    let pdf = s.get(&owner, &format!("/api/books/{book}/file/tr")).await;
    let doc = celsus_pdf::Document::load(pdf.bytes).unwrap();
    assert_eq!(
        celsus_pdf::image::page_images(&doc, &doc.pages().unwrap()[0])[0].width,
        120
    );
    let audit = s.audit_text().await;
    assert!(audit.contains("picture_changed"));
    s.finish().await;
}

fn pkce() -> (String, String) {
    let verifier = celsus_server::crypto::b64url(&celsus_server::crypto::random::<32>());
    let challenge =
        celsus_server::crypto::b64url(&celsus_server::crypto::sha256(verifier.as_bytes()));
    (verifier, challenge)
}

async fn token_request(s: &Server, form: &str) -> Reply {
    s.send_raw(
        &Browser::default(),
        Method::POST,
        "/oauth/token",
        form.as_bytes().to_vec(),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn param(url: &str, name: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| v.replace("%3A", ":").replace("%2F", "/"))
    })
}

#[tokio::test]
async fn chat_apps_connect_with_oauth_and_can_be_disconnected() {
    let s = Server::new().await;
    let owner = s.owner().await;
    upload(&s, &owner, sample_pdf("Hello world")).await;
    let redirect = "https://chat.example/oauth/callback";

    // Registration: https or this computer only.
    let bad = s
        .send_raw(
            &Browser::default(),
            Method::POST,
            "/oauth/register",
            json!({ "client_name": "Evil", "redirect_uris": ["http://evil.example/cb"] })
                .to_string()
                .into_bytes(),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let reg = s
        .send_raw(
            &Browser::default(),
            Method::POST,
            "/oauth/register",
            json!({ "client_name": "Chat App", "redirect_uris": [redirect] })
                .to_string()
                .into_bytes(),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{:?}", reg.body);
    let client = reg.body["client_id"].as_str().unwrap().to_owned();

    // The browser is sent to the web interface to approve.
    let (verifier, challenge) = pkce();
    let query = format!(
        "response_type=code&client_id={client}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&scope=read%20translate&state=xyz",
        enc(redirect)
    );
    let r = s
        .get(&Browser::default(), &format!("/oauth/authorize?{query}"))
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        r.headers
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("/#/oauth?response_type=code")
    );
    let info = s.get(&owner, &format!("/api/oauth/request?{query}")).await;
    assert_eq!(info.body["client"], "Chat App", "{:?}", info.body);
    assert_eq!(info.body["redirect_host"], "chat.example");
    // A return address the app did not register is refused before anything is asked.
    let wrong = s
        .get(
            &owner,
            &format!("/api/oauth/request?{}", query.replace("callback", "other")),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    let approve = |allow: bool| {
        json!({ "response_type": "code", "client_id": client, "redirect_uri": redirect,
        "code_challenge": challenge, "code_challenge_method": "S256", "scope": "read translate", "state": "xyz", "allow": allow })
    };
    // An API key cannot approve on a person's behalf.
    let k = key(&s, &owner, &["read", "translate"]).await;
    let r = s
        .send_raw(
            &Browser::default(),
            Method::POST,
            "/api/oauth/approve",
            approve(true).to_string().into_bytes(),
            &[
                ("authorization", &format!("Bearer {k}")),
                ("content-type", "application/json"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let denied = s.post(&owner, "/api/oauth/approve", approve(false)).await;
    let back = denied.body["redirect"].as_str().unwrap();
    assert!(
        back.starts_with(redirect)
            && back.contains("error=access_denied")
            && back.contains("state=xyz"),
        "{back}"
    );

    let approved = s.post(&owner, "/api/oauth/approve", approve(true)).await;
    let back = approved.body["redirect"].as_str().unwrap().to_owned();
    assert_eq!(param(&back, "state").as_deref(), Some("xyz"));
    let code = param(&back, "code").unwrap();
    // The code works once, only with the right verifier.
    let form = |v: &str, c: &str| {
        format!(
            "grant_type=authorization_code&code={c}&redirect_uri={}&client_id={client}&code_verifier={v}",
            enc(redirect)
        )
    };
    let wrong = token_request(&s, &form(&"x".repeat(43), &code)).await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    assert_eq!(wrong.body["error"], "invalid_grant");
    let again = token_request(&s, &form(&verifier, &code)).await;
    assert_eq!(
        again.status,
        StatusCode::BAD_REQUEST,
        "a code is spent by a failed attempt too"
    );
    let approved = s.post(&owner, "/api/oauth/approve", approve(true)).await;
    let code = param(approved.body["redirect"].as_str().unwrap(), "code").unwrap();
    let tokens = token_request(&s, &form(&verifier, &code)).await;
    assert_eq!(tokens.status, StatusCode::OK, "{:?}", tokens.body);
    assert_eq!(tokens.headers.get("cache-control").unwrap(), "no-store");
    let access = tokens.body["access_token"].as_str().unwrap().to_owned();
    let refresh = tokens.body["refresh_token"].as_str().unwrap().to_owned();
    assert_eq!(
        token_request(&s, &form(&verifier, &code)).await.status,
        StatusCode::BAD_REQUEST
    );

    // The token acts as the person over MCP.
    let books = ok(&s, &access, "list_books", json!({})).await;
    assert_eq!(books["books"].as_array().unwrap().len(), 1);
    // Refresh tokens rotate: the old one stops working.
    let renewed = token_request(
        &s,
        &format!("grant_type=refresh_token&refresh_token={refresh}&client_id={client}"),
    )
    .await;
    assert_eq!(renewed.status, StatusCode::OK, "{:?}", renewed.body);
    assert_eq!(
        token_request(
            &s,
            &format!("grant_type=refresh_token&refresh_token={refresh}")
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    let access2 = renewed.body["access_token"].as_str().unwrap().to_owned();
    assert_eq!(
        rpc(
            &s,
            &access,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    ok(&s, &access2, "list_books", json!({})).await;

    // The person sees the app and can disconnect it.
    let apps = s.get(&owner, "/api/oauth/apps").await;
    assert_eq!(apps.body["apps"][0]["name"], "Chat App", "{:?}", apps.body);
    let r = s
        .send(
            &owner,
            Method::DELETE,
            &format!("/api/oauth/apps/{client}"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        rpc(
            &s,
            &access2,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    let audit = s.audit_text().await;
    assert!(
        audit.contains("oauth_app_registered")
            && audit.contains("oauth_app_approved")
            && audit.contains("oauth_app_disconnected")
    );
    assert!(!audit.contains(&access2) && !audit.contains(&code));
    // Tokens are stored only as hashes.
    let db = s.db().await;
    let n: i64 = db
        .query_one(
            "SELECT COUNT(*) FROM oauth_tokens WHERE access_hash = $1",
            &[&access2.as_bytes()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    s.finish().await;
}

#[tokio::test]
async fn the_web_has_estimates_usage_settings_and_connection_details() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "m@lib.test", "member").await;
    let book = upload(&s, &owner, sample_pdf("Hello world")).await;
    let r = s
        .get(&owner, &format!("/api/books/{book}/estimate?pages=1-3"))
        .await;
    assert_eq!(r.body["chars"], 11, "{:?}", r.body);
    let r = s.get(&owner, "/api/usage").await;
    assert_eq!(r.body["books"], 1);
    let r = s.get(&owner, "/api/mcp").await;
    assert_eq!(r.body["url"], "https://lib.test/mcp");
    // Settings are for admins, within limits.
    assert_eq!(
        s.get(&member, "/api/admin/settings").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.get(&owner, "/api/admin/settings").await.body["batch_chars"],
        5000
    );
    let r = s
        .send(
            &owner,
            Method::PUT,
            "/api/admin/settings",
            Some(json!({ "batch_chars": 10 })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = s
        .send(
            &owner,
            Method::PUT,
            "/api/admin/settings",
            Some(json!({ "batch_chars": 8000 })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        s.get(&owner, "/api/admin/settings").await.body["batch_chars"],
        8000
    );
    assert_eq!(
        s.post(&member, "/api/admin/cache/clear", json!({}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(&owner, "/api/admin/cache/clear", json!({}))
            .await
            .status,
        StatusCode::OK
    );
    // Testing a model that is not chosen says so.
    let r = s
        .post(
            &owner,
            "/api/admin/models/test",
            json!({ "task": "translate" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Recognising a book needs a model too.
    let r = s
        .post(
            &owner,
            &format!("/api/books/{book}/recognise"),
            json!({ "lang": "tr" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    s.finish().await;
}

/// A request to `/mcp` whose body says when the server starts reading it.
async fn watched(
    s: &Server,
    token: Option<&str>,
    body: Vec<u8>,
    declared: Option<usize>,
) -> (StatusCode, bool) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, header};
    use http_body_util::{BodyExt, Full};
    use tower::ServiceExt;

    let read = Arc::new(AtomicBool::new(false));
    let seen = read.clone();
    let body = Full::new(axum::body::Bytes::from(body)).map_frame(move |frame| {
        seen.store(true, Ordering::SeqCst);
        frame
    });
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if let Some(n) = declared {
        req = req.header(header::CONTENT_LENGTH, n);
    }
    let mut req = req.body(Body::new(body)).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::new(
            "203.0.113.5".parse().unwrap(),
            50000,
        )));
    let res = s.app.clone().oneshot(req).await.unwrap();
    (res.status(), read.load(Ordering::SeqCst))
}

#[tokio::test]
async fn a_message_without_credentials_is_refused_unread() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let k = key(&s, &owner, &["read"]).await;
    let ping = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec();

    // No credentials or wrong ones: answered from the headers alone.
    let (status, read) = watched(&s, None, ping.clone(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!read, "the body of a request without credentials was read");
    let (status, read) = watched(
        &s,
        Some("celsus_0123456789abcdef_not-a-real-secret-at-all-000000000"),
        vec![b' '; 3 << 20],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!read, "the body of a request with a wrong key was read");

    // With a key the message is read and answered.
    let (status, read) = watched(&s, Some(&k), ping.clone(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(read);

    // Larger than a book can be: refused by its declared size before any of it is read, and by its real size
    // when none is declared.
    let (status, read) = watched(&s, Some(&k), ping, Some(1 << 30)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!read);
    let (status, _) = watched(&s, Some(&k), vec![b' '; 5 << 20], None).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    s.finish().await;
}

/// The cached translations of one book.
async fn cached(s: &Server, book: &str) -> Vec<String> {
    s.db()
        .await
        .query(
            "SELECT text FROM translation_cache WHERE book_id = $1",
            &[&book],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect()
}

#[tokio::test]
async fn translations_stay_with_their_book() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "uye@lib.test", "member").await;
    let line = "Chapter 7 begins here";
    let theirs = upload(&s, &member, sample_pdf(line)).await;
    let mine = upload(&s, &owner, sample_pdf(line)).await;
    let member_key = key(&s, &member, &["read", "translate"]).await;
    let owner_key = key(&s, &owner, &["read", "translate"]).await;

    // The member gives the line a translation of their own choosing, in their own book.
    let todo = ok(
        &s,
        &member_key,
        "get_text_to_translate",
        json!({ "book_id": theirs, "target_language": "tr" }),
    )
    .await;
    let block = &todo["blocks"][0];
    let sent = ok(
        &s,
        &member_key,
        "submit_translations",
        json!({ "book_id": theirs, "target_language": "tr", "accept_anyway": true,
                "translations": [{ "page": block["page"], "id": block["id"], "text": "Buraya başka bir şey yazıldı" }] }),
    )
    .await;
    assert_eq!(sent["completed_pages"], json!([1]), "{sent}");

    // The same line in the owner's book is still to be translated: nothing came over from the member's book.
    let todo = ok(
        &s,
        &owner_key,
        "get_text_to_translate",
        json!({ "book_id": mine, "target_language": "tr" }),
    )
    .await;
    let blocks = todo["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 1, "{todo}");
    assert_eq!(blocks[0]["text"], line);
    ok(
        &s,
        &owner_key,
        "submit_translations",
        json!({ "book_id": mine, "target_language": "tr",
                "translations": [{ "page": blocks[0]["page"], "id": blocks[0]["id"], "text": "Bölüm 7 burada başlıyor" }] }),
    )
    .await;
    assert_eq!(cached(&s, &theirs).await, ["Buraya başka bir şey yazıldı"]);
    assert_eq!(cached(&s, &mine).await, ["Bölüm 7 burada başlıyor"]);
    let unowned: i64 = s
        .db()
        .await
        .query_one(
            "SELECT COUNT(*) FROM translation_cache WHERE book_id IS NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(unowned, 0);

    // Translations kept before books had their own are still found, so earlier work is not lost.
    let old = "An older line of text";
    let tr: celsus_core::Language = "tr".parse().unwrap();
    s.db()
        .await
        .execute(
            "INSERT INTO translation_cache (key_hash, text) VALUES ($1, $2)",
            &[
                &celsus_server::crypto::sha256(
                    celsus_core::pipeline::cache_key(&tr, old).as_bytes(),
                ),
                &"Eski bir metin satırı",
            ],
        )
        .await
        .unwrap();
    let earlier = upload(&s, &owner, sample_pdf(old)).await;
    let todo = ok(
        &s,
        &owner_key,
        "get_text_to_translate",
        json!({ "book_id": earlier, "target_language": "tr" }),
    )
    .await;
    assert_eq!(todo["blocks"].as_array().unwrap().len(), 0, "{todo}");

    // A book's translations go with it.
    let gone = s
        .send(
            &member,
            Method::DELETE,
            &format!("/api/books/{theirs}"),
            None,
            &[],
        )
        .await;
    assert_eq!(gone.status, StatusCode::OK, "{:?}", gone.body);
    assert!(cached(&s, &theirs).await.is_empty());
    assert_eq!(cached(&s, &mine).await.len(), 1);
    s.finish().await;
}
