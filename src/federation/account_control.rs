use super::*;

/// Account authority stays at the controller even when the invocation runs remotely.
pub(super) fn account_operation(
    db: &Store,
    peer: &str,
    method: &str,
    args: &Value,
) -> Result<Value> {
    let project = args["project"].as_str().context("project required")?;
    let task = args["task"].as_str().context("owner task required")?;
    let releasing = matches!(method, "account_release" | "project_release");
    if releasing {
        crate::projects::authorize_task(db, project, task)?;
    } else {
        authorize_project_operation(db, peer, task, args)?;
    }
    let remote = args["remote_task"]
        .as_str()
        .context("remote task required")?;
    let links = db.rows(
        "SELECT remote_id,state FROM remote_links WHERE task=? AND peer=?",
        &[&task, &peer],
    )?;
    let link = links
        .first()
        .context("caller does not own this remote assignment")?;
    ensure!(
        releasing || link["state"] == "sending" || link["state"] == "running",
        "remote assignment is not active"
    );
    ensure!(
        link["remote_id"].is_null() || link["remote_id"] == remote,
        "remote assignment identity mismatch"
    );
    let request = args["request_id"]
        .as_str()
        .context("request identity required")?;
    if method == "project_acquire" {
        return Ok(
            json!({"granted":crate::accounts::reserve_project_remote(db, project, task, peer, request)?}),
        );
    }
    if method == "project_release" {
        crate::accounts::release_project_remote(db, project, task, peer, request)?;
        return Ok(json!({"released":true}));
    }
    if method == "account_release" {
        let owned: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM account_remote_reservations WHERE request_id=? AND project=? AND task=? AND runtime=?)", params![request,project,task,peer], |row| row.get(0))?;
        if !owned {
            let exists: bool = db.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM account_remote_reservations WHERE request_id=?)",
                [request],
                |row| row.get(0),
            )?;
            ensure!(!exists, "account reservation belongs to another assignment");
            return Ok(json!({"released":true}));
        }
        crate::accounts::release_remote(db, peer, request)?;
        return Ok(json!({"released":true}));
    }
    let admitted: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM project_remote_reservations WHERE request_id=? AND project=? AND task=? AND runtime=? AND state='active')", params![request,project,task,peer], |row| row.get(0))?;
    ensure!(
        admitted,
        "active project reservation required before account acquisition"
    );
    let role = args["role"].as_str().context("role required")?;
    let attempt = args["attempt"]
        .as_str()
        .context("attempt identity required")?;
    let row = db.task(task)?;
    let settings: crate::config::Settings =
        serde_json::from_str(row["settings"].as_str().context("task settings")?)?;
    let binding = crate::accounts::reserve_remote(
        db, project, task, peer, attempt, request, &settings, role,
    )?;
    let provision = binding
        .as_ref()
        .and_then(|binding| binding.account.as_deref())
        .map(|account| crate::accounts::provision(db, project, account, peer, request))
        .transpose()?;
    Ok(json!({"binding":binding,"provision":provision}))
}

pub(super) fn remove_account(db: &Store, peer: &str, method: &str, args: &Value) -> Result<Value> {
    let project = args["project"].as_str().context("project required")?;
    let task = args["task"].as_str().context("remote task required")?;
    let account = args["account"].as_str().context("account required")?;
    crate::projects::authorize_task(db, project, task)?;
    let owned: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM remote_origins WHERE task=? AND owner_peer=? AND owner_task=?)", params![task,peer,args["owner_task"].as_str().context("owner task required")?], |row| row.get(0))?;
    ensure!(owned, "caller does not own this remote assignment");
    if method == "account_retire" {
        crate::accounts::retire_received(
            db,
            project,
            account,
            args["version"]
                .as_i64()
                .context("credential version required")?,
        )
    } else {
        crate::accounts::remove_received(db, project, account)
    }
}
pub(super) async fn reconcile_credentials(db: &Store, config: &NetworkConfig) -> Result<()> {
    let pending = db.rows("SELECT d.project,d.account,d.runtime,d.version,d.state,r.task,l.remote_id FROM credential_deliveries d JOIN account_remote_reservations r ON r.request_id=d.request_id JOIN remote_links l ON l.task=r.task AND l.peer=d.runtime WHERE d.state IN ('revocation_pending','replacement_pending') AND l.remote_id IS NOT NULL LIMIT 128", &[])?;
    for row in pending {
        let peer = row["runtime"].as_str().context("credential runtime")?;
        let reply = call(config, peer, if row["state"] == "replacement_pending" { "account_retire" } else { "account_remove" }, json!({"project":row["project"],"account":row["account"],"task":row["remote_id"],"owner_task":row["task"],"version":row["version"]})).await;
        if reply.is_ok_and(|reply| reply["removed"] == true) {
            db.conn.execute("UPDATE credential_deliveries SET state='removed' WHERE project=? AND account=? AND runtime=? AND version=? AND state=?", params![row["project"].as_str(),row["account"].as_str(),peer,row["version"].as_i64(),row["state"].as_str()])?;
        }
    }
    Ok(())
}
