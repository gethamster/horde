use super::*;

/// Return true when a task must not dispatch locally, including while waiting for a VM.
pub fn route_queued(db: &Store, task: &str) -> Result<bool> {
    if !db.rows("SELECT task FROM remote_origins WHERE task=? UNION SELECT task FROM remote_links WHERE task=?", &[&task,&task])?.is_empty() { return Ok(false); }
    let project = crate::projects::task_project(db, task)?;
    let local = crate::capabilities::local_project(db, &project)?;
    let isolation: String = db.conn.query_row(
        "SELECT isolation FROM projects WHERE id=?",
        [&project],
        |row| row.get(0),
    )?;
    let policy = crate::execution_selection::policy(db, task)?;
    let requirements = policy
        .as_ref()
        .and_then(|p| p.get("requirements"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let local_allowed = crate::projects::runtime_allowed(
        db,
        &project,
        local["runtime"].as_str().context("runtime")?,
    )? && (isolation != "vm" || local["platform"]["isolation"] == "lima")
        && crate::execution_selection::check_requirements(&local, &requirements).is_ok()
        && policy.as_ref().is_none_or(|p| {
            p["selected"].is_null() || p["selected"]["runtime"] == local["runtime"]
        });
    let local_available = local["capacity"]["available"]
        .as_u64()
        .is_some_and(|slots| slots > 0)
        && local["capacity"]["draining"] != true;
    if local_allowed && local_available {
        return Ok(false);
    }
    let started: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=?)",
        [task],
        |row| row.get(0),
    )?;
    if started {
        if local_allowed {
            return Ok(false);
        }
        crate::project_runtime::queue(
            db,
            task,
            "runtime requirements changed; existing execution requires reconciliation",
        )?;
        return Ok(true);
    }
    let configured: crate::config::Settings =
        serde_json::from_str(db.task(task)?["settings"].as_str().context("settings")?)?;
    let roles = db
        .steps(task)?
        .iter()
        .map(Store::step)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|step| step.kind == "agent")
        .map(|step| step.role)
        .collect::<std::collections::BTreeSet<_>>();
    let mut hashes = serde_json::Map::new();
    for role in roles {
        hashes.insert(
            role.clone(),
            json!(crate::capabilities::configuration_hash(
                &configured.executor(&role).context("executor")?
            )?),
        );
    }
    let inventory = crate::capabilities::inventory_project(db, &project)?;
    let mut candidates = inventory["runtimes"]
        .as_array()
        .context("runtimes")?
        .iter()
        .filter(|runtime| {
            runtime["local"] != true
                && runtime["ready"] == true
                && runtime["fresh"] == true
                && runtime["capacity"]["available"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && crate::execution_selection::check_requirements(runtime, &requirements).is_ok()
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|runtime| {
        (
            runtime["capacity"]["active"].as_u64().unwrap_or(u64::MAX),
            runtime["runtime"].as_str().unwrap_or_default(),
        )
    });
    let target = candidates.into_iter().find(|runtime| {
        if let Some(policy) = &policy
            && !policy["selected"].is_null()
        {
            return policy["selected"]["runtime"] == runtime["runtime"];
        }
        hashes.iter().all(|(role, hash)| {
            runtime["capabilities"].as_array().is_some_and(|caps| {
                caps.iter()
                    .any(|cap| cap["id"] == *role && cap["configuration_hash"] == *hash)
            })
        })
    });
    let Some(target) = target else {
        crate::project_runtime::queue(
            db,
            task,
            "no granted runtime satisfies project isolation, platform, and configured execution roles",
        )?;
        return Ok(true);
    };
    let peer = target["runtime"].as_str().context("runtime identity")?;
    let row = db.task(task)?;
    let repository = Path::new(row["repo"].as_str().context("repository")?);
    if !crate::git::run(repository, &["status", "--porcelain"])?.is_empty() {
        crate::project_runtime::queue(
            db,
            task,
            "automatic remote dispatch requires a clean source repository; commit intended changes",
        )?;
        return Ok(true);
    }
    if !config(db)?
        .delegate_peers
        .iter()
        .any(|allowed| allowed == peer)
    {
        crate::project_runtime::queue(db, task, "selected runtime is not enrolled for delegation")?;
        return Ok(true);
    }
    db.atomic(|| {
        let limit: i64 = db.conn.query_row(
            "SELECT concurrency FROM projects WHERE id=?",
            [&project],
            |row| row.get(0),
        )?;
        if crate::project_runtime::active(db, &project)? >= limit {
            crate::project_runtime::queue(db, task, "project concurrency limit reached")?;
            return Ok(true);
        }
        if policy.as_ref().is_none_or(|p| p["selected"].is_null()) {
            db.conn.execute(
                "INSERT INTO external_ops VALUES(?,'federation.required_capabilities','pinned',?)",
                params![task, Value::Object(hashes).to_string()],
            )?;
        }
        db.conn.execute(
            "INSERT INTO remote_links(task,peer,state,request) VALUES(?,?,'pending',?)",
            params![task, peer, task],
        )?;
        db.conn.execute(
            "UPDATE tasks SET status='remote' WHERE id=? AND status='running'",
            [task],
        )?;
        db.conn
            .execute("DELETE FROM project_queue WHERE task=?", [task])?;
        db.event(
            task,
            "task.remote_queued",
            json!({"project":project,"runtime":peer,"automatic":true}),
        )?;
        Ok(true)
    })
}

pub(super) fn validate_required(db: &Store, project: &str, args: &Value) -> Result<()> {
    if let Some(requirements) = args.get("required_capabilities") {
        let settings = crate::config::Settings::load_project_user(db, project)?;
        for (role, hash) in requirements.as_object().context("required capabilities")? {
            let config = settings
                .executor(role)
                .context("required role is not configured on runtime")?;
            ensure!(
                crate::capabilities::configuration_hash(&config)?
                    == hash.as_str().context("configuration fingerprint")?,
                "required role configuration changed on runtime"
            );
        }
    }
    Ok(())
}
