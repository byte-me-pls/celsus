//! The database: connection pool and schema migrations.
//!
//! Every query in the server uses bound parameters; no SQL is ever built from input.

use deadpool_postgres::{Config, Pool, Runtime};
use tokio_postgres::NoTls;

pub type Client = deadpool_postgres::Object;

/// Schema changes, applied in order, each once. Never edit one that has shipped; add a new one.
const MIGRATIONS: &[&str] = &[
    // 1: accounts, sessions, second factor, invitations, API keys, throttling, audit log.
    r#"
    CREATE TABLE users (
        id BIGSERIAL PRIMARY KEY,
        email TEXT NOT NULL,
        email_lower TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,
        password_hash TEXT NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
        disabled BOOLEAN NOT NULL DEFAULT FALSE,
        totp_secret BYTEA,
        totp_pending BYTEA,
        totp_last_step BIGINT,
        created_at BIGINT NOT NULL,
        password_changed_at BIGINT NOT NULL
    );
    -- There is exactly one owner.
    CREATE UNIQUE INDEX users_one_owner ON users ((role)) WHERE role = 'owner';

    CREATE TABLE sessions (
        token_hash BYTEA PRIMARY KEY,
        public_id TEXT NOT NULL UNIQUE,
        user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        csrf TEXT NOT NULL,
        mfa_pending BOOLEAN NOT NULL,
        created_at BIGINT NOT NULL,
        last_seen BIGINT NOT NULL,
        expires_at BIGINT NOT NULL,
        ip TEXT NOT NULL,
        user_agent TEXT NOT NULL
    );
    CREATE INDEX sessions_user ON sessions (user_id);

    CREATE TABLE backup_codes (
        user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        code_hash BYTEA NOT NULL,
        used_at BIGINT,
        PRIMARY KEY (user_id, code_hash)
    );

    CREATE TABLE invites (
        token_hash BYTEA PRIMARY KEY,
        role TEXT NOT NULL CHECK (role IN ('admin', 'member')),
        created_by BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at BIGINT NOT NULL,
        expires_at BIGINT NOT NULL,
        used_at BIGINT,
        used_by BIGINT REFERENCES users(id) ON DELETE SET NULL
    );

    CREATE TABLE api_keys (
        id TEXT PRIMARY KEY,
        user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        secret_hash BYTEA NOT NULL,
        scopes TEXT[] NOT NULL,
        created_at BIGINT NOT NULL,
        expires_at BIGINT,
        last_used BIGINT,
        revoked_at BIGINT
    );
    CREATE INDEX api_keys_user ON api_keys (user_id);

    CREATE TABLE login_attempts (
        id BIGSERIAL PRIMARY KEY,
        email_lower TEXT NOT NULL,
        ip TEXT NOT NULL,
        at BIGINT NOT NULL,
        ok BOOLEAN NOT NULL
    );
    CREATE INDEX login_attempts_email ON login_attempts (email_lower, at);
    CREATE INDEX login_attempts_ip ON login_attempts (ip, at);

    CREATE TABLE audit_log (
        id BIGSERIAL PRIMARY KEY,
        at BIGINT NOT NULL,
        user_id BIGINT,
        ip TEXT NOT NULL,
        user_agent TEXT NOT NULL,
        event TEXT NOT NULL,
        target TEXT,
        details TEXT
    );
    -- The application can add to the log but never change or remove an entry.
    CREATE FUNCTION audit_log_append_only() RETURNS trigger AS $$
    BEGIN
        RAISE EXCEPTION 'the audit log is append-only';
    END
    $$ LANGUAGE plpgsql;
    CREATE TRIGGER audit_log_no_change BEFORE UPDATE OR DELETE ON audit_log
        FOR EACH ROW EXECUTE FUNCTION audit_log_append_only();
    CREATE TRIGGER audit_log_no_truncate BEFORE TRUNCATE ON audit_log
        FOR EACH STATEMENT EXECUTE FUNCTION audit_log_append_only();

    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    // 2: the library: books, translated pages, jobs, the translation cache and model providers.
    r#"
    CREATE TABLE books (
        id TEXT PRIMARY KEY,
        owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared')),
        title TEXT NOT NULL,
        author TEXT NOT NULL DEFAULT '',
        subject TEXT NOT NULL DEFAULT '',
        source_lang TEXT NOT NULL DEFAULT '',
        original_name TEXT NOT NULL,
        pages INT NOT NULL,
        bytes BIGINT NOT NULL,
        has_cover BOOLEAN NOT NULL,
        added_at BIGINT NOT NULL
    );
    CREATE INDEX books_owner ON books (owner_id);

    CREATE TABLE translated_pages (
        book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
        lang TEXT NOT NULL,
        page INT NOT NULL,
        kept INT NOT NULL DEFAULT 0,
        PRIMARY KEY (book_id, lang, page)
    );

    CREATE TABLE jobs (
        id TEXT PRIMARY KEY,
        book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
        created_by BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        lang TEXT NOT NULL,
        pages INT[] NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled')),
        done_pages INT NOT NULL DEFAULT 0,
        prompt_tokens BIGINT NOT NULL DEFAULT 0,
        completion_tokens BIGINT NOT NULL DEFAULT 0,
        model TEXT NOT NULL DEFAULT '',
        error TEXT,
        created_at BIGINT NOT NULL,
        started_at BIGINT,
        finished_at BIGINT
    );
    CREATE INDEX jobs_status ON jobs (status, created_at);
    CREATE INDEX jobs_book ON jobs (book_id);

    CREATE TABLE translation_cache (
        key_hash BYTEA PRIMARY KEY,
        text TEXT NOT NULL
    );

    CREATE TABLE providers (
        id BIGSERIAL PRIMARY KEY,
        name TEXT NOT NULL,
        url TEXT NOT NULL,
        key_sealed BYTEA,
        allow_local BOOLEAN NOT NULL DEFAULT FALSE,
        created_at BIGINT NOT NULL
    );
    "#,
];

pub fn pool(url: &str) -> Result<Pool, String> {
    let mut cfg = Config::new();
    cfg.url = Some(url.to_owned());
    cfg.create_pool(Some(Runtime::Tokio1), NoTls)
        .map_err(|e| format!("database settings: {e}"))
}

/// Bring the schema up to date. Runs inside one transaction with an advisory lock, so two servers starting at
/// once do not both migrate.
pub async fn migrate(pool: &Pool) -> Result<(), String> {
    let mut client = pool.get().await.map_err(|e| format!("database: {e}"))?;
    let tx = client.transaction().await.map_err(|e| e.to_string())?;
    tx.execute("SELECT pg_advisory_xact_lock(7243194021)", &[])
        .await
        .map_err(|e| e.to_string())?;
    tx.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INT PRIMARY KEY, applied_at BIGINT NOT NULL)",
    )
    .await
    .map_err(|e| e.to_string())?;
    let current: i32 = tx
        .query_one("SELECT COALESCE(MAX(version), 0) FROM schema_version", &[])
        .await
        .map_err(|e| e.to_string())?
        .get(0);
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = i as i32 + 1;
        if version <= current {
            continue;
        }
        tx.batch_execute(sql)
            .await
            .map_err(|e| format!("migration {version}: {e}"))?;
        tx.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES ($1, $2)",
            &[&version, &crate::now()],
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())
}
