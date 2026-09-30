//! Reservation attempts persist before requests so lost replies cannot oversubscribe.
use super::*;
pub(super) trait Client {
    async fn request(&self, p: &Policy, path: &str, body: &Value) -> Result<Value>;
}
pub(super) struct Native;
pub(super) trait StatusClient {
    async fn status(&self, p: &Policy, id: &str) -> Result<Value>;
}
impl StatusClient for Native {
    async fn status(&self, p: &Policy, id: &str) -> Result<Value> {
        process::admission(
            p,
            reqwest::Method::GET,
            &format!("/v1/reservations/{id}"),
            None,
        )
        .await
    }
}

/// Observe terminal admission state; never revoke an active publisher's lease.
pub(super) async fn reconcile(db: &Store, id: &str, c: &impl StatusClient) -> Result<bool> {
    let rows = db.rows("SELECT * FROM preview_jobs WHERE id=?", &[&id])?;
    let job = rows.first().context("preview job")?;
    let task = job["task"].as_str().context("preview task")?;
    let project = crate::projects::task_project(db, task)?;
    let (p, _, scope) = policy(db, &project)?.context("preview admission policy missing")?;
    let Some(record) = super::lease_receipts::recorded(db, job, &scope)? else {
        return Ok(false);
    };
    let rid = record["reservation"]
        .as_str()
        .context("admission reservation")?;
    ensure!(
        rid.len() == 64
            && rid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid admission reservation identity"
    );
    let r = c.status(&p, rid).await?;
    let identity = &r["identity"];
    ensure!(
        r["schema_version"] == 1
            && r["id"] == rid
            && identity["schema_version"] == 1
            && identity["scope"] == scope
            && identity["project_id"] == project
            && identity["run_id"] == task
            && identity["built_commit"] == job["head"]
            && identity["recipe_hash"] == job["recipe"],
        "admission status identity mismatch"
    );
    let key = identity["idempotency_key"]
        .as_str()
        .context("admission request identity")?;
    ensure!(record["key"] == key, "admission request identity mismatch");
    if let Some(request) = record["request"].as_str() {
        ensure!(
            serde_json::from_str::<Value>(request)? == *identity,
            "admission frozen request identity mismatch"
        );
    }
    let Some(state) = r["state"]
        .as_str()
        .filter(|s| matches!(*s, "admitted" | "held" | "released" | "expired"))
    else {
        return Ok(false);
    };
    super::lease_receipts::settle(db, job, &record, state, identity)
}
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
        let mut request = identity.clone();
        request["idempotency_key"] = json!(key);
        let expected = super::lease_receipts::intent(db, id, ordinal, &request)?;
        let r = c.request(p, "/v1/reservations", &request).await?;
        ensure!(
            r["schema_version"] == 1 && r["id"] == expected && r["identity"] == request,
            "registry admission identity mismatch"
        );
        if matches!(r["state"].as_str(), Some("expired" | "released")) {
            super::lease_receipts::release(db, id, &expected, r["state"].as_str().unwrap())?;
            ordinal += 1;
            continue;
        }
        if r["state"] == "held" {
            super::lease_receipts::release(db, id, &expected, "held")?;
        }
        ensure!(
            r["state"] == "admitted",
            "publication held by registry admission"
        );
        let rid = r["id"].as_str().context("reservation id")?;
        ensure!(rid == expected, "admission reservation identity mismatch");
        db.conn.execute(
            "UPDATE preview_admission_receipts SET state='admitted' WHERE job=? AND ordinal=?",
            params![id, ordinal],
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
        super::lease_receipts::release(db, id, rid, r["state"].as_str().unwrap())?;
    }
    Ok(())
}
