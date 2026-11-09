//! Attacks on the server, run against a real PostgreSQL. Each test gets its own empty database.
//!
//! Needs `CELSUS_TEST_DATABASE_URL` (see `common`).

mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use common::*;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tower::ServiceExt;

// ---------------------------------------------------------------- setup

#[tokio::test]
async fn a_fresh_server_cannot_be_claimed_without_the_setup_code() {
    let s = Server::new().await;
    let anon = Browser::default();
    assert_eq!(s.get(&anon, "/api/setup").await.body["needed"], true);
    let r = s.post(&anon, "/api/setup", json!({"code": "AAAAAA-BBBBBB-CCCCCC-DDDDDD", "email": "evil@x.test", "name": "Evil", "password": PASSWORD})).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.cookie().is_none());
    let owner = s.owner().await;
    assert!(owner.cookie.is_some());
    // Once there is an owner, setup is closed even with a fresh code.
    let code = celsus_server::routes::setup::prepare(&s.state)
        .await
        .unwrap();
    assert!(code.is_none(), "no new setup code once an owner exists");
    let r = s
        .post(
            &anon,
            "/api/setup",
            json!({"code": "x", "email": "evil@x.test", "name": "Evil", "password": PASSWORD}),
        )
        .await;
    assert_ne!(r.status, StatusCode::OK);
    assert_eq!(s.get(&anon, "/api/setup").await.body["needed"], false);
    assert!(s.events().await.contains(&"setup_failed".into()));
    s.finish().await;
}

#[tokio::test]
async fn setup_code_guessing_is_throttled() {
    let s = Server::new().await;
    let anon = Browser::default();
    let mut last = StatusCode::OK;
    for _ in 0..6 {
        last = s
            .post(
                &anon,
                "/api/setup",
                json!({"code": "wrong", "email": "a@x.test", "name": "A", "password": PASSWORD}),
            )
            .await
            .status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    s.finish().await;
}

// ---------------------------------------------------------------- signing in

#[tokio::test]
async fn wrong_and_unknown_accounts_look_the_same() {
    let s = Server::new().await;
    s.owner().await;
    let wrong = s
        .login("owner@lib.test", "not-the-password", "198.51.100.1")
        .await;
    let unknown = s
        .login("nobody@lib.test", "not-the-password", "198.51.100.2")
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status, unknown.status);
    assert_eq!(
        wrong.body, unknown.body,
        "same message, so emails cannot be discovered"
    );
    assert!(wrong.cookie().is_none());
    // Odd input is just a failed sign-in, not an error inside the server.
    let sql = s
        .login(
            "' OR 1=1; DROP TABLE users; --",
            "x' OR '1'='1",
            "198.51.100.3",
        )
        .await;
    assert_eq!(sql.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.login("owner@lib.test", PASSWORD, "198.51.100.4")
            .await
            .status,
        StatusCode::OK,
        "users table intact"
    );
    let huge = s
        .login("owner@lib.test", &"x".repeat(5000), "198.51.100.5")
        .await;
    assert_eq!(huge.status, StatusCode::UNAUTHORIZED);
    s.finish().await;
}

#[tokio::test]
async fn password_guessing_is_slowed_down_per_account() {
    let s = Server::new().await;
    s.owner().await;
    for i in 0..5 {
        let r = s
            .login(
                "owner@lib.test",
                &format!("guess-number-{i}"),
                "198.51.100.10",
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    // Now even the right password waits, from any address.
    let r = s.login("OWNER@lib.test", PASSWORD, "192.0.2.77").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers.get(header::RETRY_AFTER).is_some());
    assert!(s.events().await.contains(&"login_throttled".into()));
    // When the wait is over, the right password works again (the account was never locked).
    s.db()
        .await
        .execute("UPDATE login_attempts SET at = at - 3600", &[])
        .await
        .unwrap();
    assert_eq!(
        s.login("owner@lib.test", PASSWORD, "192.0.2.77")
            .await
            .status,
        StatusCode::OK
    );
    s.finish().await;
}

#[tokio::test]
async fn one_address_cannot_spray_many_accounts_and_cannot_forge_its_address() {
    let s = Server::new().await;
    s.owner().await;
    let db = s.db().await;
    let t = celsus_server::now();
    for i in 0..30 {
        db.execute("INSERT INTO login_attempts (email_lower, ip, at, ok) VALUES ($1, '198.51.100.20', $2, FALSE)", &[&format!("user{i}@x.test"), &t])
            .await
            .unwrap();
    }
    let blocked = s.login("owner@lib.test", PASSWORD, "198.51.100.20").await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    // A made-up X-Forwarded-For from an untrusted peer changes nothing.
    let b = Browser {
        ip: Some("198.51.100.20"),
        ..Default::default()
    };
    let forged = s
        .send(
            &b,
            Method::POST,
            "/api/auth/login",
            Some(json!({"email": "owner@lib.test", "password": PASSWORD})),
            &[("x-forwarded-for", "1.1.1.1")],
        )
        .await;
    assert_eq!(forged.status, StatusCode::TOO_MANY_REQUESTS);
    // Through the trusted proxy, the real client address is used.
    let b = Browser {
        ip: Some("10.0.0.2"),
        ..Default::default()
    };
    let via_proxy = s
        .send(
            &b,
            Method::POST,
            "/api/auth/login",
            Some(json!({"email": "owner@lib.test", "password": PASSWORD})),
            &[("x-forwarded-for", "192.0.2.50")],
        )
        .await;
    assert_eq!(via_proxy.status, StatusCode::OK);
    s.finish().await;
}

// ---------------------------------------------------------------- sessions

#[tokio::test]
async fn session_cookies_are_locked_down_and_stored_only_as_hashes() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let r = s.login("owner@lib.test", PASSWORD, "198.51.100.30").await;
    let set = r
        .headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set.starts_with("__Host-celsus_session="));
    for flag in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(set.contains(flag), "{flag} missing in {set}");
    }
    assert!(!set.contains("Domain="), "__Host- cookies have no Domain");
    let token = owner.cookie.clone().unwrap();
    let db = s.db().await;
    let stored: Vec<Vec<u8>> = db
        .query("SELECT token_hash FROM sessions", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert!(
        stored
            .iter()
            .all(|h| h.len() == 32 && h != token.as_bytes())
    );
    let as_text: i64 = db
        .query_one("SELECT COUNT(*) FROM sessions WHERE encode(token_hash, 'escape') LIKE '%' || $1 || '%'", &[&token])
        .await
        .unwrap()
        .get(0);
    assert_eq!(as_text, 0);
    // A guessed or made-up cookie gets nothing.
    let fake = Browser {
        cookie: Some(celsus_server::crypto::token()),
        ..Default::default()
    };
    assert_eq!(
        s.get(&fake, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    s.finish().await;
}

#[tokio::test]
async fn signing_out_ends_the_session_for_good() {
    let s = Server::new().await;
    let owner = s.owner().await;
    assert_eq!(s.get(&owner, "/api/auth/me").await.status, StatusCode::OK);
    let r = s.post(&owner, "/api/auth/logout", json!({})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    // The old cookie, replayed, is dead.
    assert_eq!(
        s.get(&owner, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    s.finish().await;
}

#[tokio::test]
async fn sessions_expire_when_idle_and_after_their_lifetime() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let db = s.db().await;
    db.execute(
        "UPDATE sessions SET last_seen = last_seen - $1",
        &[&(celsus_server::auth::IDLE + 10)],
    )
    .await
    .unwrap();
    assert_eq!(
        s.get(&owner, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    let fresh = s.login("owner@lib.test", PASSWORD, "198.51.100.31").await;
    let b = Browser {
        cookie: fresh.cookie(),
        ..Default::default()
    };
    db.execute(
        "UPDATE sessions SET expires_at = $1",
        &[&(celsus_server::now() - 1)],
    )
    .await
    .unwrap();
    assert_eq!(
        s.get(&b, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    s.finish().await;
}

#[tokio::test]
async fn changing_the_password_signs_out_everywhere_else() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let other = s.login("owner@lib.test", PASSWORD, "198.51.100.32").await;
    let other = Browser {
        cookie: other.cookie(),
        ..Default::default()
    };
    let wrong = s
        .post(
            &owner,
            "/api/auth/password",
            json!({"current": "nope-nope-nope", "new": "yeni-parola-2026-mavi"}),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let weak = s
        .post(
            &owner,
            "/api/auth/password",
            json!({"current": PASSWORD, "new": "1234567890"}),
        )
        .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    let r = s
        .post(
            &owner,
            "/api/auth/password",
            json!({"current": PASSWORD, "new": "yeni-parola-2026-mavi"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        s.get(&other, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.get(&owner, "/api/auth/me").await.status,
        StatusCode::OK,
        "the session that changed it stays"
    );
    assert_eq!(
        s.login("owner@lib.test", PASSWORD, "198.51.100.33")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.login("owner@lib.test", "yeni-parola-2026-mavi", "198.51.100.33")
            .await
            .status,
        StatusCode::OK
    );
    s.finish().await;
}

// ---------------------------------------------------------------- cross-site

#[tokio::test]
async fn forged_cross_site_requests_are_refused() {
    let s = Server::new().await;
    let owner = s.owner().await;
    // No CSRF token.
    let no_token = Browser {
        csrf: None,
        ..owner.clone()
    };
    let r = s
        .post(&no_token, "/api/admin/invites", json!({"role": "member"}))
        .await;
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("csrf"))
    );
    // A wrong one.
    let bad = Browser {
        csrf: Some("x".repeat(43)),
        ..owner.clone()
    };
    assert_eq!(
        s.post(&bad, "/api/admin/invites", json!({})).await.status,
        StatusCode::FORBIDDEN
    );
    // From another site, even with the right token.
    let r = s
        .send(
            &owner,
            Method::POST,
            "/api/admin/invites",
            Some(json!({})),
            &[("origin", "https://evil.test")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = s
        .send(
            &owner,
            Method::POST,
            "/api/admin/invites",
            Some(json!({})),
            &[("sec-fetch-site", "cross-site")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Signing in from another site is refused too (login CSRF).
    let r = s
        .send(
            &Browser::default(),
            Method::POST,
            "/api/auth/login",
            Some(json!({"email": "owner@lib.test", "password": PASSWORD})),
            &[("origin", "https://evil.test")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Reading is fine without a token; the right token makes writes work.
    assert_eq!(
        s.get(&no_token, "/api/auth/me").await.status,
        StatusCode::OK
    );
    assert_eq!(
        s.post(&owner, "/api/admin/invites", json!({})).await.status,
        StatusCode::OK
    );
    s.finish().await;
}

// ---------------------------------------------------------------- two-step sign-in

#[tokio::test]
async fn two_step_sign_in_end_to_end() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let setup = s.post(&owner, "/api/auth/two-step/setup", json!({})).await;
    assert_eq!(setup.status, StatusCode::OK);
    let secret = base32_decode(setup.body["secret"].as_str().unwrap());
    assert!(
        setup.body["url"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/Celsus:owner%40lib.test")
            || setup.body["url"]
                .as_str()
                .unwrap()
                .starts_with("otpauth://totp/Celsus:owner@lib.test")
    );
    assert_eq!(
        s.post(
            &owner,
            "/api/auth/two-step/enable",
            json!({"code": "000000"})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    let enabled = s
        .post(
            &owner,
            "/api/auth/two-step/enable",
            json!({"code": code_now(&secret, 0)}),
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{:?}", enabled.body);
    let backups: Vec<String> = enabled.body["backup_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(backups.len(), 10);

    // The secret is encrypted in the database.
    let stored: Vec<u8> = s
        .db()
        .await
        .query_one("SELECT totp_secret FROM users", &[])
        .await
        .unwrap()
        .get(0);
    assert!(!stored.windows(secret.len()).any(|w| w == secret.as_slice()));

    // Password alone gives a pending session that can do nothing else.
    let r = s.login("owner@lib.test", PASSWORD, "198.51.100.40").await;
    assert_eq!(r.body["two_step_required"], true);
    let pending = Browser {
        cookie: r.cookie(),
        ..Default::default()
    };
    assert_eq!(
        s.get(&pending, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.get(&pending, "/api/admin/users").await.status,
        StatusCode::UNAUTHORIZED
    );
    // The code used to turn it on cannot be used again.
    assert_eq!(
        s.post(
            &pending,
            "/api/auth/two-step",
            json!({"code": code_now(&secret, 0)})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    let ok = s
        .post(
            &pending,
            "/api/auth/two-step",
            json!({"code": code_now(&secret, 1)}),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    let full = Browser {
        cookie: ok.cookie(),
        csrf: ok.body["csrf"].as_str().map(str::to_owned),
        ip: None,
    };
    assert_ne!(
        full.cookie, pending.cookie,
        "a new token after the second step"
    );
    assert_eq!(s.get(&full, "/api/auth/me").await.status, StatusCode::OK);
    assert_eq!(
        s.get(&pending, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED,
        "the pending token is gone"
    );

    // A backup code works once.
    let r = s.login("owner@lib.test", PASSWORD, "198.51.100.41").await;
    let p2 = Browser {
        cookie: r.cookie(),
        ..Default::default()
    };
    assert_eq!(
        s.post(
            &p2,
            "/api/auth/two-step",
            json!({"code": backups[0].to_uppercase()})
        )
        .await
        .status,
        StatusCode::OK
    );
    let r = s.login("owner@lib.test", PASSWORD, "198.51.100.42").await;
    let p3 = Browser {
        cookie: r.cookie(),
        ..Default::default()
    };
    assert_eq!(
        s.post(&p3, "/api/auth/two-step", json!({"code": backups[0]}))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    // Guessing codes is throttled like passwords.
    let mut last = StatusCode::OK;
    for _ in 0..6 {
        last = s
            .post(&p3, "/api/auth/two-step", json!({"code": "111111"}))
            .await
            .status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);

    // Turning it off needs both the password and a code.
    assert_eq!(
        s.post(
            &full,
            "/api/auth/two-step/disable",
            json!({"password": PASSWORD, "code": "123456"})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(
            &full,
            "/api/auth/two-step/disable",
            json!({"password": "wrong-password!", "code": backups[1]})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(
            &full,
            "/api/auth/two-step/disable",
            json!({"password": PASSWORD, "code": backups[2]})
        )
        .await
        .status,
        StatusCode::OK
    );
    let events = s.events().await;
    for e in ["two_step_enabled", "two_step_failed", "two_step_disabled"] {
        assert!(events.contains(&e.to_string()), "{e}");
    }
    s.finish().await;
}

#[tokio::test]
async fn a_pending_session_expires_quickly() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let setup = s.post(&owner, "/api/auth/two-step/setup", json!({})).await;
    let secret = base32_decode(setup.body["secret"].as_str().unwrap());
    s.post(
        &owner,
        "/api/auth/two-step/enable",
        json!({"code": code_now(&secret, 0)}),
    )
    .await;
    let r = s.login("owner@lib.test", PASSWORD, "198.51.100.43").await;
    let pending = Browser {
        cookie: r.cookie(),
        ..Default::default()
    };
    s.db()
        .await
        .execute(
            "UPDATE sessions SET expires_at = $1 WHERE mfa_pending",
            &[&(celsus_server::now() - 1)],
        )
        .await
        .unwrap();
    assert_eq!(
        s.post(
            &pending,
            "/api/auth/two-step",
            json!({"code": code_now(&secret, 1)})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    s.finish().await;
}

// ---------------------------------------------------------------- roles

#[tokio::test]
async fn members_cannot_reach_admin_actions_or_raise_themselves() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "uye@lib.test", "member").await;
    assert_eq!(
        s.get(&member, "/api/admin/users").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.get(&member, "/api/admin/audit").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(&member, "/api/admin/invites", json!({}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let me = s.get(&member, "/api/auth/me").await.body["user"]["id"]
        .as_i64()
        .unwrap();
    let r = s
        .send(
            &member,
            Method::PATCH,
            &format!("/api/admin/users/{me}"),
            Some(json!({"role": "admin"})),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    s.finish().await;
}

#[tokio::test]
async fn admins_manage_members_but_not_the_owner_or_other_admins() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let admin = s.member(&owner, "admin@lib.test", "admin").await;
    let admin2 = s.member(&owner, "admin2@lib.test", "admin").await;
    let member = s.member(&admin, "uye@lib.test", "member").await;
    let id = |b: &Browser| {
        let b = b.clone();
        let s = &s;
        async move {
            s.get(&b, "/api/auth/me").await.body["user"]["id"]
                .as_i64()
                .unwrap()
        }
    };
    let (owner_id, admin2_id, member_id) = (id(&owner).await, id(&admin2).await, id(&member).await);
    // Admins cannot make admins.
    assert_eq!(
        s.post(&admin, "/api/admin/invites", json!({"role": "admin"}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let patch = |b: &Browser, uid: i64, body: Value| {
        let b = b.clone();
        let s = &s;
        async move {
            s.send(
                &b,
                Method::PATCH,
                &format!("/api/admin/users/{uid}"),
                Some(body),
                &[],
            )
            .await
            .status
        }
    };
    assert_eq!(
        patch(&admin, member_id, json!({"role": "admin"})).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        patch(&admin, owner_id, json!({"disabled": true})).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        patch(&admin, admin2_id, json!({"disabled": true})).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        patch(&owner, owner_id, json!({"disabled": true})).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        patch(&owner, member_id, json!({"role": "owner"})).await,
        StatusCode::BAD_REQUEST,
        "there is only one owner"
    );
    // Disabling cuts the member off at once.
    assert_eq!(
        patch(&admin, member_id, json!({"disabled": true})).await,
        StatusCode::OK
    );
    assert_eq!(
        s.get(&member, "/api/auth/me").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.login("uye@lib.test", PASSWORD, "198.51.100.50")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // The owner can promote.
    assert_eq!(
        patch(
            &owner,
            member_id,
            json!({"role": "admin", "disabled": false})
        )
        .await,
        StatusCode::OK
    );
    let owners: i64 = s
        .db()
        .await
        .query_one("SELECT COUNT(*) FROM users WHERE role = 'owner'", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(owners, 1);
    s.finish().await;
}

#[tokio::test]
async fn invitations_are_single_use_and_expire() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let inv = s
        .post(&owner, "/api/admin/invites", json!({"role": "member"}))
        .await;
    let token = inv.body["token"].as_str().unwrap().to_owned();
    let accept =
        |email: &str| json!({"token": token, "email": email, "name": "X", "password": PASSWORD});
    let anon = Browser::default();
    assert_eq!(
        s.post(&anon, "/api/invites/accept", accept("one@lib.test"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        s.post(&anon, "/api/invites/accept", accept("two@lib.test"))
            .await
            .status,
        StatusCode::BAD_REQUEST,
        "used"
    );
    let inv = s.post(&owner, "/api/admin/invites", json!({})).await;
    let token2 = inv.body["token"].as_str().unwrap().to_owned();
    s.db()
        .await
        .execute(
            "UPDATE invites SET expires_at = $1 WHERE used_at IS NULL",
            &[&(celsus_server::now() - 1)],
        )
        .await
        .unwrap();
    let r = s
        .post(
            &anon,
            "/api/invites/accept",
            json!({"token": token2, "email": "three@lib.test", "name": "X", "password": PASSWORD}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "expired");
    let made_up = s.post(&anon, "/api/invites/accept", json!({"token": celsus_server::crypto::token(), "email": "four@lib.test", "name": "X", "password": PASSWORD})).await;
    assert_eq!(made_up.status, StatusCode::BAD_REQUEST);
    // An invitation cannot make an owner, whatever is sent.
    let inv = s
        .post(&owner, "/api/admin/invites", json!({"role": "owner"}))
        .await;
    assert_eq!(inv.status, StatusCode::BAD_REQUEST);
    // Invitation tokens are stored only as hashes.
    let raw: i64 = s
        .db()
        .await
        .query_one(
            "SELECT COUNT(*) FROM invites WHERE encode(token_hash, 'escape') LIKE '%' || $1 || '%'",
            &[&token],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(raw, 0);
    s.finish().await;
}

// ---------------------------------------------------------------- API keys

#[tokio::test]
async fn api_keys_are_scoped_revocable_and_hashed() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "uye@lib.test", "member").await;
    // A member cannot give a key more than they have.
    assert_eq!(
        s.post(
            &member,
            "/api/keys",
            json!({"name": "x", "scopes": ["admin"]})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(
            &member,
            "/api/keys",
            json!({"name": "x", "scopes": ["root"]})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    let created = s
        .post(
            &member,
            "/api/keys",
            json!({"name": "script", "scopes": ["read"]}),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    let key = created.body["key"].as_str().unwrap().to_owned();
    let id = created.body["id"].as_str().unwrap().to_owned();
    let bearer = format!("Bearer {key}");
    let with_key = |path: &str, method: Method, body: Option<Value>| {
        let bearer = bearer.clone();
        let path = path.to_owned();
        let s = &s;
        async move {
            s.send(
                &Browser::default(),
                method,
                &path,
                body,
                &[("authorization", bearer.as_str())],
            )
            .await
        }
    };
    let me = with_key("/api/auth/me", Method::GET, None).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["user"]["email"], "uye@lib.test");
    // A key cannot make keys, cannot reach admin pages, cannot change the password.
    assert_eq!(
        with_key(
            "/api/keys",
            Method::POST,
            Some(json!({"name": "y", "scopes": ["read"]}))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        with_key("/api/admin/users", Method::GET, None).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        with_key(
            "/api/auth/password",
            Method::POST,
            Some(json!({"current": PASSWORD, "new": "baska-bir-parola-99"}))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // Tampered keys fail.
    let mut tampered = key.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    let r = s
        .send(
            &Browser::default(),
            Method::GET,
            "/api/auth/me",
            None,
            &[("authorization", &format!("Bearer {tampered}"))],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    for junk in [
        "Bearer",
        "Bearer ",
        "Basic abc",
        "Bearer celsus_",
        "Bearer celsus_zzzzzzzzzzzzzzzz_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let r = s
            .send(
                &Browser::default(),
                Method::GET,
                "/api/auth/me",
                None,
                &[("authorization", junk)],
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{junk}");
    }
    // Stored as a hash only.
    let secret = key.rsplit_once('_').unwrap().1.to_owned();
    let raw: i64 = s.db().await.query_one("SELECT COUNT(*) FROM api_keys WHERE encode(secret_hash, 'escape') LIKE '%' || $1 || '%'", &[&secret]).await.unwrap().get(0);
    assert_eq!(raw, 0);
    // Another member cannot revoke it; the owner (admin) can see it disappear when revoked by its owner.
    let other = s.member(&owner, "baska@lib.test", "member").await;
    assert_eq!(
        s.send(
            &other,
            Method::DELETE,
            &format!("/api/keys/{id}"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.send(
            &member,
            Method::DELETE,
            &format!("/api/keys/{id}"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        with_key("/api/auth/me", Method::GET, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Expired keys and keys of disabled accounts stop working.
    let k2 = s
        .post(
            &member,
            "/api/keys",
            json!({"name": "k2", "scopes": ["read", "translate"], "expires_in_days": 30}),
        )
        .await;
    let k2 = k2.body["key"].as_str().unwrap().to_owned();
    let auth2 = format!("Bearer {k2}");
    assert_eq!(
        s.send(
            &Browser::default(),
            Method::GET,
            "/api/auth/me",
            None,
            &[("authorization", &auth2)]
        )
        .await
        .status,
        StatusCode::OK
    );
    s.db()
        .await
        .execute(
            "UPDATE api_keys SET expires_at = $1",
            &[&(celsus_server::now() - 1)],
        )
        .await
        .unwrap();
    assert_eq!(
        s.send(
            &Browser::default(),
            Method::GET,
            "/api/auth/me",
            None,
            &[("authorization", &auth2)]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    s.db()
        .await
        .execute("UPDATE api_keys SET expires_at = NULL", &[])
        .await
        .unwrap();
    let mid = s.get(&member, "/api/auth/me").await.body["user"]["id"]
        .as_i64()
        .unwrap();
    s.send(
        &owner,
        Method::PATCH,
        &format!("/api/admin/users/{mid}"),
        Some(json!({"disabled": true})),
        &[],
    )
    .await;
    assert_eq!(
        s.send(
            &Browser::default(),
            Method::GET,
            "/api/auth/me",
            None,
            &[("authorization", &auth2)]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    s.finish().await;
}

// ---------------------------------------------------------------- audit log

#[tokio::test]
async fn the_audit_log_records_events_holds_no_secrets_and_cannot_be_changed() {
    let s = Server::new().await;
    let owner = s.owner().await;
    s.login("owner@lib.test", "wrong-password-123", "198.51.100.60")
        .await;
    let key = s
        .post(
            &owner,
            "/api/keys",
            json!({"name": "k", "scopes": ["read"]}),
        )
        .await
        .body["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let inv = s.post(&owner, "/api/admin/invites", json!({})).await.body["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let events = s.events().await;
    for e in ["setup", "login_failed", "key_created", "invite_created"] {
        assert!(events.contains(&e.to_string()), "{e} missing: {events:?}");
    }
    let text = s.audit_text().await;
    for secret in [
        PASSWORD,
        "wrong-password-123",
        key.as_str(),
        inv.as_str(),
        owner.cookie.as_deref().unwrap(),
        owner.csrf.as_deref().unwrap(),
    ] {
        assert!(!text.contains(secret), "a secret leaked into the audit log");
    }
    let log = s.get(&owner, "/api/admin/audit?limit=5").await;
    assert_eq!(log.status, StatusCode::OK);
    assert!(log.body["entries"].as_array().unwrap().len() <= 5);
    // The database itself refuses to change or remove entries.
    let db = s.db().await;
    assert!(
        db.execute("UPDATE audit_log SET event = 'nothing'", &[])
            .await
            .is_err()
    );
    assert!(db.execute("DELETE FROM audit_log", &[]).await.is_err());
    assert!(db.batch_execute("TRUNCATE audit_log").await.is_err());
    s.finish().await;
}

// ---------------------------------------------------------------- responses

#[tokio::test]
async fn responses_carry_security_headers_and_hide_internals() {
    let s = Server::new().await;
    let anon = Browser::default();
    let r = s.get(&anon, "/api/setup").await;
    let h = |k: &str| {
        r.headers
            .get(k)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default()
    };
    assert!(h("content-security-policy").contains("default-src 'self'"));
    assert!(h("content-security-policy").contains("frame-ancestors 'none'"));
    assert_eq!(h("x-content-type-options"), "nosniff");
    assert_eq!(h("x-frame-options"), "DENY");
    assert_eq!(h("referrer-policy"), "no-referrer");
    assert_eq!(h("cache-control"), "no-store");
    assert!(
        h("strict-transport-security").contains("max-age="),
        "served over https"
    );
    assert!(h("server").is_empty(), "no server banner");
    // The interface files are revalidated on every load, so an updated server is never paired with old scripts.
    for path in ["/", "/web.js", "/i18n/core.js"] {
        let r = s.get(&anon, path).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
        assert_eq!(
            r.headers.get("cache-control").unwrap(),
            "no-cache",
            "{path}"
        );
    }
    // Unknown paths: a plain 404, also with headers.
    let r = s.get(&anon, "/api/../../etc/passwd").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.headers.get("content-security-policy").is_some());
    // Oversized bodies are refused before they are read.
    let big = json!({"email": "a@b.c", "password": "x".repeat(2 << 20)});
    let r = s.post(&anon, "/api/auth/login", big).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    // Malformed JSON is a client error.
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 1))));
    let res = s.app.clone().oneshot(req).await.unwrap();
    assert!(res.status().is_client_error());
    s.finish().await;
}

#[tokio::test]
async fn a_member_sees_only_their_own_sessions_and_cannot_end_others() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "uye@lib.test", "member").await;
    let owner_sessions = s.get(&owner, "/api/auth/sessions").await.body["sessions"]
        .as_array()
        .unwrap()
        .clone();
    let owner_session = owner_sessions[0]["id"].as_str().unwrap().to_owned();
    let mine = s.get(&member, "/api/auth/sessions").await.body["sessions"]
        .as_array()
        .unwrap()
        .clone();
    assert!(mine.iter().all(|x| x["id"] != owner_session.as_str()));
    let r = s
        .send(
            &member,
            Method::DELETE,
            &format!("/api/auth/sessions/{owner_session}"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(s.get(&owner, "/api/auth/me").await.status, StatusCode::OK);
    s.finish().await;
}

#[tokio::test]
async fn admins_see_when_members_were_last_active_after_they_sign_out() {
    let s = Server::new().await;
    let owner = s.owner().await;
    let member = s.member(&owner, "reader@example.org", "member").await;
    let r = s.post(&member, "/api/auth/logout", json!({})).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = s.get(&owner, "/api/admin/users").await;
    let users = r.body["users"].as_array().unwrap();
    let reader = users
        .iter()
        .find(|u| u["email"] == "reader@example.org")
        .unwrap();
    assert!(reader["last_seen"].as_i64().is_some(), "{reader}");
    s.finish().await;
}
