//! The library: who sees and changes which book, uploads, files, jobs and model connections.
//!
//! Needs `CELSUS_TEST_DATABASE_URL` (see `common`).

mod common;

use axum::http::{Method, StatusCode, header};
use common::*;
use serde_json::{Value, json};

async fn upload(s: &Server, b: &Browser, pdf: Vec<u8>, query: &str) -> Reply {
    s.send_raw(
        b,
        Method::POST,
        &format!("/api/books{query}"),
        pdf,
        &[
            ("x-filename", "Sample%20Book.pdf"),
            ("content-type", "application/pdf"),
        ],
    )
    .await
}

fn id_of(r: &Reply) -> String {
    r.body["book"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{:?}", r.body))
        .to_owned()
}

#[tokio::test]
async fn uploads_are_checked() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let ok = upload(&s, &owner, sample_pdf("Hello world"), "").await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    assert_eq!(ok.body["book"]["title"], "Sample Book");
    assert_eq!(ok.body["book"]["author"], "Test Author");
    assert_eq!(
        ok.body["book"]["visibility"], "private",
        "private unless shared"
    );
    assert_eq!(ok.body["book"]["pages"], 1);
    let id = id_of(&ok);
    assert_eq!(id.len(), 32);

    // Not a PDF, a broken PDF, too big, wrong visibility, no write scope.
    assert_eq!(
        upload(&s, &owner, b"MZ\x90\0this is a program".to_vec(), "")
            .await
            .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        upload(&s, &owner, b"%PDF-1.7\n garbage".to_vec(), "")
            .await
            .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut huge = sample_pdf("x");
    huge.resize(3 << 20, b' ');
    assert_eq!(
        upload(&s, &owner, huge, "").await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        upload(&s, &owner, sample_pdf("x"), "?visibility=public")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let key = s
        .post(
            &owner,
            "/api/keys",
            json!({"name": "ro", "scopes": ["read"]}),
        )
        .await
        .body["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = s
        .send_raw(
            &Browser::default(),
            Method::POST,
            "/api/books",
            sample_pdf("x"),
            &[("authorization", &format!("Bearer {key}"))],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "a read-only key cannot add books"
    );
    // A name with path parts cannot escape the library.
    let r = s
        .send_raw(
            &owner,
            Method::POST,
            "/api/books",
            sample_pdf("x"),
            &[("x-filename", "..%2F..%2Fserver.key")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        !r.body["book"]["original_name"]
            .as_str()
            .unwrap()
            .contains('/')
    );
    assert!(
        s.state
            .config
            .data_dir
            .join("server.key")
            .metadata()
            .unwrap()
            .len()
            == 32,
        "server key untouched"
    );
    // The file is stored as sent and comes back the same, as a PDF that is not sniffed.
    let f = s.get(&owner, &format!("/api/books/{id}/file/source")).await;
    assert_eq!(f.status, StatusCode::OK);
    assert_eq!(f.bytes, sample_pdf("Hello world"));
    assert_eq!(f.headers[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(f.headers["x-content-type-options"], "nosniff");
    assert!(s.events().await.contains(&"book_added".into()));
    s.finish().await;
}

#[tokio::test]
async fn private_books_are_invisible_to_everyone_else() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let ali = s.member(&owner, "ali@lib.test", "member").await;
    let veli = s.member(&owner, "veli@lib.test", "member").await;
    let id = id_of(&upload(&s, &ali, sample_pdf("Private"), "").await);

    // Another member and even the server owner see nothing: not in lists, every path says "not found".
    for b in [&veli, &owner] {
        let list = s.get(b, "/api/books").await;
        assert!(list.body["books"].as_array().unwrap().is_empty());
        for path in [
            format!("/api/books/{id}"),
            format!("/api/books/{id}/file/source"),
            format!("/api/books/{id}/cover"),
            format!("/api/books/{id}/pages/tr"),
        ] {
            assert_eq!(
                s.get(b, &path).await.status,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        assert_eq!(
            s.post(
                b,
                &format!("/api/books/{id}/jobs"),
                json!({"lang": "tr", "pages": [1]})
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            s.send(
                b,
                Method::PATCH,
                &format!("/api/books/{id}"),
                Some(json!({"title": "mine now"})),
                &[]
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            s.send(b, Method::DELETE, &format!("/api/books/{id}"), None, &[])
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
    // The same answer as for a book that does not exist.
    let none = s
        .get(&veli, "/api/books/0123456789abcdef0123456789abcdef")
        .await;
    let hidden = s.get(&veli, &format!("/api/books/{id}")).await;
    assert_eq!(none.body, hidden.body);
    assert_eq!(
        s.get(&ali, &format!("/api/books/{id}")).await.status,
        StatusCode::OK
    );
    s.finish().await;
}

#[tokio::test]
async fn shared_books_are_read_by_all_and_changed_by_the_owner() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let ali = s.member(&owner, "ali@lib.test", "member").await;
    let veli = s.member(&owner, "veli@lib.test", "member").await;
    let id = id_of(&upload(&s, &ali, sample_pdf("Shared"), "?visibility=shared").await);
    let r = s.get(&veli, &format!("/api/books/{id}")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["book"]["editable"], false);
    assert_eq!(
        s.get(&veli, &format!("/api/books/{id}/file/source"))
            .await
            .status,
        StatusCode::OK
    );
    // Readers may translate it, not change or remove it, and not make it private.
    assert_eq!(
        s.send(
            &veli,
            Method::PATCH,
            &format!("/api/books/{id}"),
            Some(json!({"title": "x"})),
            &[]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.send(
            &veli,
            Method::DELETE,
            &format!("/api/books/{id}"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // An admin can tidy shared books but not decide who sees them.
    assert_eq!(
        s.send(
            &owner,
            Method::PATCH,
            &format!("/api/books/{id}"),
            Some(json!({"title": "Better title"})),
            &[]
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        s.send(
            &owner,
            Method::PATCH,
            &format!("/api/books/{id}"),
            Some(json!({"visibility": "private"})),
            &[]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // The owner makes it private again and it disappears for others.
    assert_eq!(
        s.send(
            &ali,
            Method::PATCH,
            &format!("/api/books/{id}"),
            Some(json!({"visibility": "private"})),
            &[]
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        s.get(&veli, &format!("/api/books/{id}")).await.status,
        StatusCode::NOT_FOUND
    );
    // Odd input is refused or cleaned.
    assert_eq!(
        s.send(
            &ali,
            Method::PATCH,
            &format!("/api/books/{id}"),
            Some(json!({"title": "  \u{0}\u{7} "})),
            &[]
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.send(
            &ali,
            Method::PATCH,
            &format!("/api/books/{id}"),
            Some(json!({"source_lang": "../../x"})),
            &[]
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    let r = s
        .send(
            &ali,
            Method::PATCH,
            &format!("/api/books/{id}"),
            Some(json!({"title": "<script>alert(1)</script>"})),
            &[],
        )
        .await;
    assert_eq!(
        r.body["book"]["title"], "<script>alert(1)</script>",
        "stored as text; the page escapes it"
    );
    // Removing deletes the files too.
    assert_eq!(
        s.send(&ali, Method::DELETE, &format!("/api/books/{id}"), None, &[])
            .await
            .status,
        StatusCode::OK
    );
    assert!(!s.state.config.library_dir().join(&id).exists());
    s.finish().await;
}

#[tokio::test]
async fn paths_and_languages_cannot_reach_other_files() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let id = id_of(&upload(&s, &owner, sample_pdf("x"), "").await);
    for path in [
        format!("/api/books/{id}/file/..%2F..%2Fserver.key"),
        format!("/api/books/{id}/file/..%5Cserver.key"),
        format!("/api/books/{id}/file/tr.pdf"),
        format!("/api/books/{id}/file/TR"),
        "/api/books/..%2F..%2F/file/source".into(),
        "/api/books/%2E%2E/file/source".into(),
        format!("/api/books/{}/file/source", id.to_uppercase()),
        format!("/api/books/{id}/pages/..%2F..%2Fx"),
    ] {
        let r = s.get(&owner, &path).await;
        assert!(
            r.status == StatusCode::NOT_FOUND || r.status == StatusCode::BAD_REQUEST,
            "{path}: {}",
            r.status
        );
        assert!(
            !r.bytes.windows(4).any(|w| w == b"%PDF")
                || path.contains("source") && r.status == StatusCode::OK
        );
    }
    // The web files are served, but never anything outside their folder.
    for path in [
        "/../Cargo.toml",
        "/..%2FCargo.toml",
        "/%2e%2e/Cargo.toml",
        "/....//Cargo.toml",
    ] {
        let r = s.get(&owner, path).await;
        assert!(
            !String::from_utf8_lossy(&r.bytes).contains("[package]"),
            "{path}"
        );
    }
    s.finish().await;
}

#[tokio::test]
async fn jobs_are_checked_and_only_their_people_can_stop_them() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let ali = s.member(&owner, "ali@lib.test", "member").await;
    let veli = s.member(&owner, "veli@lib.test", "member").await;
    let id = id_of(&upload(&s, &ali, sample_pdf("x"), "?visibility=shared").await);
    let path = format!("/api/books/{id}/jobs");
    // Without a model nothing can be translated, and the answer says so instead of queueing a job that fails.
    let r = s.post(&ali, &path, json!({"lang": "tr", "pages": [1]})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body["error"], "no_model");
    assert!(
        s.get(&ali, "/api/jobs").await.body["jobs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    choose_fake_model(&s, &owner).await;
    for bad in [
        json!({"lang": "tr", "pages": []}),
        json!({"lang": "tr", "pages": [0]}),
        json!({"lang": "tr", "pages": [2]}),
        json!({"lang": "../x", "pages": [1]}),
        json!({"lang": "TR;DROP", "pages": [1]}),
    ] {
        assert_eq!(
            s.post(&ali, &path, bad.clone()).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    let job = s
        .post(&ali, &path, json!({"lang": "tr", "pages": [1, 1, 1]}))
        .await;
    assert_eq!(job.status, StatusCode::OK);
    let job_id = job.body["id"].as_str().unwrap().to_owned();
    // Veli can see it on a shared book but not stop it; the owner (admin) can.
    assert!(
        s.get(&veli, "/api/jobs").await.body["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == job_id.as_str())
    );
    let r = s
        .post(&veli, &format!("/api/jobs/{job_id}/cancel"), json!({}))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = s
        .post(&owner, &format!("/api/jobs/{job_id}/cancel"), json!({}))
        .await;
    assert!(
        r.status == StatusCode::OK || r.status == StatusCode::BAD_REQUEST,
        "{:?}",
        r.body
    );
    assert_eq!(
        s.post(&veli, "/api/jobs/not-an-id/cancel", json!({}))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    s.finish().await;
}

#[tokio::test]
async fn model_connections_are_for_admins_and_cannot_reach_the_local_network() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "uye@lib.test", "member").await;
    assert_eq!(
        s.get(&member, "/api/admin/providers").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(
            &member,
            "/api/admin/providers",
            json!({"name": "x", "url": "https://8.8.8.8/v1"})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    for url in [
        "http://127.0.0.1:11434/v1",
        "http://localhost/v1",
        "http://169.254.169.254/latest",
        "http://10.0.0.1/v1",
        "http://[::1]/v1",
        "file:///etc/passwd",
        "https://user:pw@8.8.8.8/v1",
    ] {
        let r = s
            .post(
                &owner,
                "/api/admin/providers",
                json!({"name": "x", "url": url}),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{url}");
    }
    // Marked local on purpose, a LAN server is allowed.
    let r = s
        .post(
            &owner,
            "/api/admin/providers",
            json!({"name": "Ollama", "url": "http://127.0.0.1:11434/v1", "allow_local": true}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // Keys are encrypted and never sent back.
    let r = s
        .post(
            &owner,
            "/api/admin/providers",
            json!({"name": "OpenAI", "url": "https://8.8.8.8/v1", "key": "sk-very-secret-key-123"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let pid = r.body["id"].as_i64().unwrap();
    let list = s.get(&owner, "/api/admin/providers").await;
    assert!(!list.bytes.windows(10).any(|w| w == b"sk-very-se"));
    assert_eq!(list.body["providers"][1]["has_key"], true);
    let stored: Vec<u8> = s
        .db()
        .await
        .query_one("SELECT key_sealed FROM providers WHERE id = $1", &[&pid])
        .await
        .unwrap()
        .get(0);
    assert!(!stored.windows(10).any(|w| w == b"sk-very-se"));
    assert!(!s.audit_text().await.contains("sk-very-secret"));
    // Changing an address re-checks it.
    let r = s
        .send(
            &owner,
            Method::PATCH,
            &format!("/api/admin/providers/{pid}"),
            Some(json!({"url": "http://192.168.1.1/v1"})),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Choosing models.
    assert_eq!(
        s.send(
            &member,
            Method::PUT,
            "/api/admin/models",
            Some(json!({"task": "translate", "provider_id": pid, "model": "m"})),
            &[]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.send(
            &owner,
            Method::PUT,
            "/api/admin/models",
            Some(json!({"task": "translate", "provider_id": 999, "model": "m"})),
            &[]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.send(
            &owner,
            Method::PUT,
            "/api/admin/models",
            Some(json!({"task": "translate", "provider_id": pid, "model": "gpt-5.5-mini"})),
            &[]
        )
        .await
        .status,
        StatusCode::OK
    );
    s.finish().await;
}

/// A stand-in for an OpenAI-style server that "translates" every block by prefixing it.
async fn fake_model() -> String {
    use axum::{Json, Router, routing::post};
    async fn chat(Json(req): Json<Value>) -> Json<Value> {
        let user: Value =
            serde_json::from_str(req["messages"][1]["content"].as_str().unwrap_or("{}"))
                .unwrap_or_default();
        let items: Vec<Value> = user["blocks"]
            .as_array()
            .map(|b| b.iter().map(|x| json!({"id": x["id"], "text": format!("TR {}", x["text"].as_str().unwrap_or(""))})).collect())
            .unwrap_or_default();
        Json(
            json!({"choices": [{"message": {"content": json!({"translations": items}).to_string()}}], "usage": {"prompt_tokens": 10, "completion_tokens": 5}}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(chat)),
        )
        .await
        .unwrap()
    });
    format!("http://{addr}/v1")
}

/// Make the stand-in model the one that translates.
async fn choose_fake_model(s: &Server, owner: &Browser) {
    let url = fake_model().await;
    let pid = s
        .post(
            owner,
            "/api/admin/providers",
            json!({"name": "Fake", "url": url, "allow_local": true}),
        )
        .await
        .body["id"]
        .as_i64()
        .unwrap();
    s.send(
        owner,
        Method::PUT,
        "/api/admin/models",
        Some(json!({"task": "translate", "provider_id": pid, "model": "fake"})),
        &[],
    )
    .await;
}

#[tokio::test]
async fn a_job_translates_end_to_end() {
    let s = Server::new().await;
    let owner = s.owner().await;
    choose_fake_model(&s, &owner).await;
    let id = id_of(&upload(&s, &owner, sample_pdf("Hello world"), "").await);
    let job = s
        .post(
            &owner,
            &format!("/api/books/{id}/jobs"),
            json!({"lang": "tr", "pages": [1]}),
        )
        .await;
    let job_id = job.body["id"].as_str().unwrap().to_owned();
    let mut status = String::new();
    for _ in 0..200 {
        let jobs = s.get(&owner, "/api/jobs").await;
        let j = jobs.body["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == job_id.as_str())
            .unwrap()
            .clone();
        status = j["status"].as_str().unwrap().to_owned();
        if status == "done" || status == "failed" {
            assert_eq!(status, "done", "{:?}", j["error"]);
            assert_eq!(j["tokens"], 15);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(status, "done");
    let pages = s.get(&owner, &format!("/api/books/{id}/pages/tr")).await;
    assert_eq!(pages.body["pages"], json!([[1, 0]]));
    let pdf = s.get(&owner, &format!("/api/books/{id}/file/tr")).await;
    assert_eq!(pdf.status, StatusCode::OK);
    assert!(pdf.bytes.starts_with(b"%PDF"));
    // The translation is in the cache, so translating again costs nothing.
    let cached: i64 = s
        .db()
        .await
        .query_one("SELECT COUNT(*) FROM translation_cache", &[])
        .await
        .unwrap()
        .get(0);
    assert!(cached >= 1);
    let book = s.get(&owner, &format!("/api/books/{id}")).await;
    assert_eq!(book.body["book"]["translated"]["tr"], 1);
    s.finish().await;
}

#[tokio::test]
async fn the_language_list_covers_the_world_and_every_code_is_accepted() {
    let s = Server::new().await;
    let r = s.get(&Browser::default(), "/api/languages").await;
    assert_eq!(r.status, StatusCode::OK);
    let list = r.body.as_array().unwrap();
    assert!(list.len() > 100, "{} languages", list.len());
    let codes: Vec<&str> = list.iter().map(|l| l["code"].as_str().unwrap()).collect();
    for want in [
        "tr", "en", "ar", "he", "hi", "th", "zh-hans", "zh-hant", "ja", "ko", "sw", "pt-br",
    ] {
        assert!(codes.contains(&want), "{want} missing");
    }
    assert!(list.iter().any(|l| l["code"] == "ar" && l["rtl"] == true));
    // Every listed code can be used for a translation.
    for code in codes {
        assert!(
            celsus_server::library::valid_lang(code),
            "{code} is refused"
        );
    }
    s.finish().await;
}
