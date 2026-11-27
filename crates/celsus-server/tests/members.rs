//! What admins do about members beyond inviting them: deleting an account and deciding what becomes of its
//! books, clearing a disabled member's private books, limits for members, and a password reset from the server's
//! command line.
//!
//! Needs `CELSUS_TEST_DATABASE_URL` (see `common`).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{Value, json};

async fn add(s: &Server, b: &Browser, text: &str, visibility: &str) -> Reply {
    s.send_raw(
        b,
        Method::POST,
        &format!("/api/books?visibility={visibility}"),
        sample_pdf(text),
        &[
            ("x-filename", "Book.pdf"),
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

async fn user_id(s: &Server, admin: &Browser, email: &str) -> Value {
    let users = s.get(admin, "/api/admin/users").await;
    users.body["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["email"] == email)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn an_account_is_deleted_and_its_shared_books_stay() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let admin = s.member(&owner, "admin@lib.test", "admin").await;
    let member = s.member(&owner, "member@lib.test", "member").await;
    let private = id_of(&add(&s, &member, "Mine alone", "private").await);
    let shared = id_of(&add(&s, &member, "For everyone", "shared").await);
    let u = user_id(&s, &admin, "member@lib.test").await;
    assert_eq!(
        (u["private_books"].clone(), u["shared_books"].clone()),
        (json!(1), json!(1))
    );
    let id = u["id"].as_i64().unwrap();

    // Nobody deletes themselves, the owner, or (unless they are the owner) another admin.
    let me = user_id(&s, &admin, "admin@lib.test").await["id"]
        .as_i64()
        .unwrap();
    let owner_id = user_id(&s, &admin, "owner@lib.test").await["id"]
        .as_i64()
        .unwrap();
    for (who, target) in [(&admin, me), (&admin, owner_id), (&member, me)] {
        let r = s
            .send(
                who,
                Method::DELETE,
                &format!("/api/admin/users/{target}"),
                None,
                &[],
            )
            .await;
        assert_ne!(r.status, StatusCode::OK, "{target}");
    }

    let r = s
        .send(
            &admin,
            Method::DELETE,
            &format!("/api/admin/users/{id}"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        (
            r.body["private_books_deleted"].clone(),
            r.body["shared_books_moved"].clone()
        ),
        (json!(1), json!(1))
    );
    assert!(user_id(&s, &admin, "member@lib.test").await.is_null());
    // The shared book is the admin's now; the private one is gone with its files.
    let b = s.get(&admin, &format!("/api/books/{shared}")).await;
    assert_eq!(b.body["book"]["mine"], true, "{:?}", b.body);
    let gone: i64 = s
        .db()
        .await
        .query_one("SELECT COUNT(*) FROM books WHERE id = $1", &[&private])
        .await
        .unwrap()
        .get(0);
    assert_eq!(gone, 0);
    assert!(!s.state.config.library_dir().join(&private).exists());
    // The member's session ended with the account.
    assert_eq!(
        s.get(&member, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        s.audit_text()
            .await
            .contains("user_deleted member@lib.test")
    );
    s.finish().await;
}

#[tokio::test]
async fn a_disabled_members_private_books_can_be_removed() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "member@lib.test", "member").await;
    let private = id_of(&add(&s, &member, "Mine alone", "private").await);
    let shared = id_of(&add(&s, &member, "For everyone", "shared").await);
    let id = user_id(&s, &owner, "member@lib.test").await["id"]
        .as_i64()
        .unwrap();
    let path = format!("/api/admin/users/{id}/books");
    // Not while the account is in use.
    let r = s.send(&owner, Method::DELETE, &path, None, &[]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    let r = s
        .send(
            &owner,
            Method::PATCH,
            &format!("/api/admin/users/{id}"),
            Some(json!({ "disabled": true })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = s.send(&owner, Method::DELETE, &path, None, &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["books_removed"], 1);
    let db = s.db().await;
    let left: Vec<String> = db
        .query("SELECT id FROM books", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(left, vec![shared]);
    assert!(!left.contains(&private));
    s.finish().await;
}

#[tokio::test]
async fn members_are_held_to_the_limits_an_admin_sets() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "member@lib.test", "member").await;
    let r = s.get(&owner, "/api/admin/settings").await;
    assert_eq!(
        (
            r.body["max_books"].clone(),
            r.body["max_queued_pages"].clone()
        ),
        (json!(0), json!(0))
    );
    let r = s
        .send(
            &owner,
            Method::PUT,
            "/api/admin/settings",
            Some(json!({ "max_books": -1 })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = s
        .send(
            &owner,
            Method::PUT,
            "/api/admin/settings",
            Some(json!({ "max_books": 1, "max_queued_pages": 1 })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    // The batch size is left as it was.
    let r = s.get(&owner, "/api/admin/settings").await;
    assert_eq!(
        (
            r.body["max_books"].clone(),
            r.body["max_queued_pages"].clone(),
            r.body["batch_chars"].clone()
        ),
        (json!(1), json!(1), json!(5000))
    );
    // A member may have one book; admins and the owner are not held to it.
    let book = id_of(&add(&s, &member, "First", "private").await);
    let r = add(&s, &member, "Second", "private").await;
    assert_eq!(
        (r.status, r.body["error"].clone()),
        (StatusCode::FORBIDDEN, json!("book_limit")),
        "{:?}",
        r.body
    );
    assert_eq!(
        add(&s, &owner, "One", "private").await.status,
        StatusCode::OK
    );
    assert_eq!(
        add(&s, &owner, "Two", "private").await.status,
        StatusCode::OK
    );
    // One page waiting at most: a job for one page is taken, the next is not.
    s.db()
        .await
        .execute(
            "INSERT INTO settings (key, value) VALUES ('model.translate', '999:none') ON CONFLICT (key) DO UPDATE SET value = '999:none'",
            &[],
        )
        .await
        .unwrap();
    // A job that is waiting, as the queue would see it.
    let member_id = user_id(&s, &owner, "member@lib.test").await["id"]
        .as_i64()
        .unwrap();
    s.db()
        .await
        .execute(
            "INSERT INTO jobs (id, book_id, created_by, lang, pages, status, created_at) VALUES ('0123456789abcdef0123456789abcdef', $1, $2, 'de', '{1}', 'queued', 0)",
            &[&book, &member_id],
        )
        .await
        .unwrap();
    let r = s
        .post(
            &member,
            &format!("/api/books/{book}/jobs"),
            json!({ "lang": "tr", "pages": [1] }),
        )
        .await;
    assert_eq!(
        (r.status, r.body["error"].clone()),
        (StatusCode::TOO_MANY_REQUESTS, json!("page_limit")),
        "{:?}",
        r.body
    );
    // 0 is no limit.
    s.send(
        &owner,
        Method::PUT,
        "/api/admin/settings",
        Some(json!({ "max_books": 0, "max_queued_pages": 0 })),
        &[],
    )
    .await;
    assert_eq!(
        add(&s, &member, "Second", "private").await.status,
        StatusCode::OK
    );
    s.finish().await;
}

#[tokio::test]
async fn a_password_is_reset_from_the_command_line() {
    let s = Server::new().await;
    let owner = s.owner().await;
    // Two-step sign-in on, then the phone is lost.
    let setup = s.post(&owner, "/api/auth/two-step/setup", json!({})).await;
    let secret = base32_decode(setup.body["secret"].as_str().unwrap());
    let r = s
        .post(
            &owner,
            "/api/auth/two-step/enable",
            json!({ "code": code_now(&secret, 0) }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);

    let db = s.db().await;
    assert!(
        celsus_server::routes::admin::reset_password(&**db, "nobody@lib.test", false)
            .await
            .is_err()
    );
    let reset = celsus_server::routes::admin::reset_password(&**db, " Owner@Lib.test ", false)
        .await
        .unwrap();
    assert!(reset.password.len() >= 30 && reset.sessions_ended >= 1);
    // The old password is gone, the old session ended; two-step sign-in is still asked for.
    assert_eq!(
        s.get(&owner, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_ne!(
        s.login("owner@lib.test", PASSWORD, "198.51.100.1")
            .await
            .status,
        StatusCode::OK
    );
    let r = s
        .login("owner@lib.test", &reset.password, "198.51.100.2")
        .await;
    assert_eq!(r.body["two_step_required"], true, "{:?}", r.body);

    // With two-step sign-in turned off too, the new password alone signs in.
    let reset = celsus_server::routes::admin::reset_password(&**db, "owner@lib.test", true)
        .await
        .unwrap();
    assert!(reset.two_step_off);
    let r = s
        .login("owner@lib.test", &reset.password, "198.51.100.3")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert!(r.body["two_step_required"] != true, "{:?}", r.body);
    let audit = s.audit_text().await;
    assert!(audit.contains("password_reset owner@lib.test"), "{audit}");
    assert!(!audit.contains(&reset.password));
    s.finish().await;
}
