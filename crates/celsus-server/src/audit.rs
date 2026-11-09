//! The audit log: who did what, when and from where. Append-only (the database refuses changes), and it never
//! holds passwords, codes or tokens.

use serde_json::Value;
use tokio_postgres::GenericClient;

use crate::web::{ApiResult, Client};

pub async fn record(
    db: &impl GenericClient,
    actor: Option<i64>,
    client: &Client,
    event: &str,
    target: Option<&str>,
    details: Value,
) -> ApiResult<()> {
    let details = (!details.is_null()).then(|| details.to_string());
    db.execute(
        "INSERT INTO audit_log (at, user_id, ip, user_agent, event, target, details) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        &[&crate::now(), &actor, &client.ip, &client.user_agent, &event, &target, &details],
    )
    .await?;
    Ok(())
}
