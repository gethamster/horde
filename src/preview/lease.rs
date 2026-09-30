//! Reservation attempts persist before requests so lost replies cannot oversubscribe.
use super::*;
pub(super) trait Client {
    async fn request(&self, p: &Policy, path: &str, body: &Value) -> Result<Value>;
}
pub(super) struct Native;
impl Client for Native {
    async fn request(&self, p: &Policy, path: &str, body: &Value) -> Result<Value> {
        process::admission(p, reqwest::Method::POST, path, Some(body)).await
    }
}
pub(super) async fn reserve(
    db: &Store,
    id: &str,
    p: &Policy,
    identity: &Value,
    c: &impl Client,
) -> Result<String> {
    let mut ordinal: i64 = db.conn.query_row(
        "SELECT COALESCE(MAX(ordinal),0) FROM preview_admissions WHERE job=?",
        [id],
        |r| r.get(0),
    )?;
    if ordinal == 0 {
        ordinal = 1;
    }
    for _ in 0..2 {
        let key = format!("{id}-reservation-{ordinal}");
        db.conn.execute(
            "INSERT OR IGNORE INTO preview_admissions VALUES(?,?,?)",
            params![id, ordinal, key],
        )?;
        let mut request = identity.clone();
        request["idempotency_key"] = json!(key);
        let r = c.request(p, "/v1/reservations", &request).await?;
        ensure!(
            r["identity"] == request,
            "registry admission identity mismatch"
        );
        if matches!(r["state"].as_str(), Some("expired" | "released")) {
            ordinal += 1;
            continue;
        }
        ensure!(
            r["state"] == "admitted",
            "publication held by registry admission"
        );
        let rid = r["id"].as_str().context("reservation id")?;
        ensure!(
            !rid.is_empty()
                && rid.len() <= 200
                && rid
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid reservation id"
        );
        db.conn.execute(
            "UPDATE preview_jobs SET reservation=? WHERE id=?",
            params![rid, id],
        )?;
        return Ok(rid.to_owned());
    }
    anyhow::bail!("registry reservation expired repeatedly; reconcile admission service")
}
pub(super) async fn release(db: &Store, id: &str, p: &Policy, c: &impl Client) -> Result<()> {
    let row = db.rows("SELECT reservation FROM preview_jobs WHERE id=?", &[&id])?;
    if let Some(rid) = row.first().and_then(|r| r["reservation"].as_str()) {
        let r = c
            .request(p, &format!("/v1/reservations/{rid}/release"), &json!({}))
            .await?;
        ensure!(
            matches!(r["state"].as_str(), Some("released" | "expired")),
            "registry admission release unconfirmed"
        );
        db.conn
            .execute("UPDATE preview_jobs SET reservation=NULL WHERE id=?", [id])?;
    }
    Ok(())
}
