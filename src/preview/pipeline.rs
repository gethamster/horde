use super::*;
use super::{
    process::{admission, helper},
    review::{reviewed, schedule_review},
};
use std::time::Duration;
use tokio::task::JoinHandle;
#[derive(Default)]
pub struct Queue {
    active: Option<JoinHandle<Result<()>>>,
    next_scan: Option<std::time::Instant>,
}
impl Queue {
    /// Publication occupies controller capacity, while reap/cancel still run.
    pub async fn tick(&mut self, db: &Store) -> Result<bool> {
        migrate(db)?;
        if self.active.as_ref().is_some_and(|h| h.is_finished()) {
            let h = self.active.take().unwrap();
            if !matches!(h.await, Ok(Ok(()))) {
                db.conn.execute("UPDATE preview_jobs SET phase='held',error='publication interrupted; reconcile provenance before retry' WHERE phase NOT IN ('succeeded','held','superseded')",[])?;
            }
        }
        if self.active.is_some() {
            return Ok(true);
        }
        let workers: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE state IN ('running','uncertain')",
            [],
            |r| r.get(0),
        )?;
        if workers > 0 || crate::storage::status(db)?["pressure"] == true {
            return Ok(false);
        }
        // Resumable intent already exists; replay observes publication/checkpoint/push
        // under their existing idempotency and exact-head guards.
        let draining = crate::management::draining(db)?;
        let queued=db.rows("SELECT id,task FROM preview_jobs WHERE phase NOT IN ('succeeded','held','superseded') AND (?=0 OR phase!='queued') ORDER BY created LIMIT 1",&[&draining])?;
        let mut job = queued.first().cloned();
        let scan_due = self
            .next_scan
            .is_none_or(|t| std::time::Instant::now() >= t);
        if job.is_none() && !draining && scan_due {
            self.next_scan = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
            for task in db.rows("SELECT t.id FROM tasks t JOIN task_projects tp ON tp.task=t.id JOIN preview_policies p ON p.project=tp.project WHERE t.status='succeeded' AND json_extract(p.policy,'$.enabled')=1 AND EXISTS(SELECT 1 FROM events e WHERE e.task=t.id AND e.seq>p.activation_seq AND e.kind IN ('task.submitted','workflow.revised','step.started')) ORDER BY t.created",&[])? {
                let task=task["id"].as_str().context("task")?;
                let root=db.root.clone();
                let queued_task=task.to_owned();
                let result=crate::budget::blocking_timeout(Duration::from_secs(60),move||enqueue(&Store::open(&root)?,&queued_task)).await;
                match result {
                    Ok(Some(id))=>{job=Some(json!({"id":id,"task":task}));break},
                    Ok(None)=>(),
                    Err(error)=>{
                        let text=error.to_string();
                        let prior=db.rows("SELECT data FROM events WHERE task=? AND kind='run.preview_held' ORDER BY seq DESC LIMIT 1",&[&task])?;
                        if prior.first().and_then(|r|r["data"].as_str()).and_then(|s|serde_json::from_str::<Value>(s).ok()).is_none_or(|v|v["error"]!=text) {db.event(task,"run.preview_held",json!({"error":text}))?;}
                    }
                }
            }
        }
        if let Some(job) = job {
            let root = db.root.clone();
            let id = job["id"].as_str().context("job id")?.to_owned();
            phase(db, &id, "preparing", None)?;
            self.active = Some(tokio::task::spawn_local(async move {
                let db = Store::open(&root)?;
                if let Err(error) = advance(&db, &id).await {
                    phase(&db, &id, "held", Some(&error.to_string()))?;
                }
                Ok(())
            }));
            return Ok(true);
        }
        Ok(false)
    }
    pub async fn shutdown(self, db: &Store) -> Result<()> {
        if let Some(h) = self.active {
            h.abort();
            let _ = h.await;
            db.conn.execute("UPDATE preview_jobs SET error='controller stopped; external effects require reconciliation' WHERE phase NOT IN ('succeeded','held','superseded')",[])?;
        }
        Ok(())
    }
}
pub(super) fn identity(db: &Store, task: &str) -> Result<(String, String)> {
    let path = crate::git::task_workspace(db, task)?;
    ensure!(
        crate::git::run(&path, &["status", "--porcelain"])?.is_empty(),
        "Run workspace changed during preview"
    );
    Ok((
        crate::git::run(&path, &["rev-parse", "HEAD"])?,
        crate::git::run(&path, &["rev-parse", "HEAD^{tree}"])?,
    ))
}
pub(super) fn enqueue(db: &Store, task: &str) -> Result<Option<String>> {
    let project = crate::projects::task_project(db, task)?;
    let Some((p, generation, _)) = policy(db, &project)? else {
        return Ok(None);
    };
    if !p.enabled {
        return Ok(None);
    }
    let context = crate::run::run_context(db, task)?;
    ensure!(
        context["thread_id"].as_str().is_some() && context["brief_id"].as_str().is_some(),
        "automatic preview requires Thread and Brief"
    );
    let (head, tree) = identity(db, task)?;
    let main = crate::run::main_head(db, task)?["commit_sha"]
        .as_str()
        .context("main")?
        .to_owned();
    if reviewed(db, task, &head, &tree, &generation)?.as_deref() != Some(&main) {
        schedule_review(db, task, &p)?;
        return Ok(None);
    }
    let recipe = p.recipe_hash();
    let id = crate::store::hash(&serde_json::to_vec(&json!([
        task, head, tree, main, generation, recipe
    ]))?);
    let inserted = db.conn.execute(
        "INSERT OR IGNORE INTO preview_jobs VALUES(?,?,?,?,?,?,?,'queued',NULL,NULL,NULL,?)",
        params![
            id,
            task,
            generation,
            head,
            tree,
            main,
            recipe,
            crate::store::now()
        ],
    )?;
    if inserted == 0 {
        return Ok(None);
    }
    db.conn.execute("UPDATE preview_jobs SET phase='superseded' WHERE task=? AND id!=? AND phase NOT IN ('succeeded','superseded')",params![task,id])?;
    db.event(task,"run.preview_phase",json!({"id":id,"phase":"queued","commit_sha":head,"tree_sha":tree,"recipe_hash":recipe,"generation":generation}))?;
    Ok(Some(id))
}
fn phase(db: &Store, id: &str, state: &str, error: Option<&str>) -> Result<()> {
    db.atomic(|| {
        db.conn.execute(
            "UPDATE preview_jobs SET phase=?,error=? WHERE id=?",
            params![state, error, id],
        )?;
        let task: String =
            db.conn
                .query_row("SELECT task FROM preview_jobs WHERE id=?", [id], |r| {
                    r.get(0)
                })?;
        db.event(
            &task,
            "run.preview_phase",
            json!({"id":id,"phase":state,"error":error}),
        )?;
        Ok(())
    })
}
pub(super) fn fresh(db: &Store, j: &Value, p: &Policy, generation: &str) -> Result<()> {
    let task = j["task"].as_str().context("task")?;
    let (head, tree) = identity(db, task)?;
    let (current, current_generation, _) =
        policy(db, &p.project_id)?.context("preview policy removed during publication")?;
    ensure!(
        p.enabled
            && current.enabled
            && current_generation == generation
            && j["generation"] == generation
            && j["head"] == head
            && j["tree"] == tree
            && db.task(task)?["status"] == "succeeded",
        "preview revision superseded; rerun review"
    );
    ensure!(
        crate::run::main_head(db, task)?["commit_sha"] == j["main_head"],
        "main advanced after review; rerun review"
    );
    ensure!(
        reviewed(db, task, &head, &tree, generation)?.as_deref() == j["main_head"].as_str(),
        "preview review no longer current"
    );
    Ok(())
}
async fn check_fresh(db: &Store, job: &Value, policy: &Policy, generation: &str) -> Result<()> {
    let root = db.root.clone();
    let job = job.clone();
    let policy = policy.clone();
    let generation = generation.to_owned();
    crate::budget::blocking_timeout(Duration::from_secs(policy.timeout_seconds), move || {
        fresh(&Store::open(&root)?, &job, &policy, &generation)
    })
    .await
}
pub(super) async fn advance(db: &Store, id: &str) -> Result<()> {
    let rows = db.rows("SELECT * FROM preview_jobs WHERE id=?", &[&id])?;
    let j = rows.first().context("job")?;
    let task = j["task"].as_str().context("task")?;
    let project = crate::projects::task_project(db, task)?;
    let (p, generation, scope) = policy(db, &project)?.context("preview policy removed")?;
    check_fresh(db, j, &p, &generation).await?;
    ensure!(
        std::env::var_os("HORDE_RUN_ATTESTATION_KEY").is_some(),
        "automatic preview requires controller signing key"
    );
    let head = j["head"].as_str().context("head")?;
    let tree = j["tree"].as_str().context("tree")?;
    let mut request = json!({"scope":scope,"project_id":p.project_id,"project_slug":p.project_slug,"component":p.component,"run_id":task,"built_commit":head,"tree_sha":tree,"builder_image":p.builder_image,"runtime_image":p.runtime_image,"dockerfile":p.dockerfile,"registry_admission":null,"mode":"probe"});
    let workspace = crate::git::task_workspace(db, task)?;
    phase(db, id, "validating", None)?;
    let validated = crate::executor::run_command_env(
        &p.validation,
        &workspace,
        p.timeout_seconds,
        None,
        &crate::secrets::values(db, task)?,
    )
    .await?;
    ensure!(
        validated["success"] == true,
        "preview validation failed before publication"
    );
    check_fresh(db, j, &p, &generation).await?;
    db.event(
        task,
        "run.preview_validated",
        json!({"id":id,"commit_sha":head,"tree_sha":tree,"validation":p.validation}),
    )?;
    phase(db, id, "publishing", None)?;
    let probe = helper(p.clone(), request.clone(), &workspace)
        .await
        .context("probe helper")?;
    let mut reservation = None;
    if probe["reused"] != true {
        let identity = json!({"schema_version":1,"idempotency_key":id,"scope":scope,"project_id":p.project_id,"run_id":task,"built_commit":head,"recipe_hash":p.recipe_hash(),"estimated_publish_bytes":p.estimated_publish_bytes});
        let rid = super::lease::reserve(db, id, &p, &identity, &super::lease::Native).await?;
        request["registry_admission"] = json!({"reservation_id":rid,"endpoint":p.admission_url,"token_file":p.admission_token_file});
        reservation = Some(rid);
    }
    request["mode"] = json!("automated");
    let mut build = Box::pin(helper(p.clone(), request, &workspace));
    let publication = loop {
        tokio::select! {result=&mut build=>break result,_=tokio::time::sleep(std::time::Duration::from_secs(60)),if reservation.is_some()=>{let path=format!("/v1/reservations/{}/renew",reservation.as_deref().unwrap());let r=admission(&p,reqwest::Method::POST,&path,Some(&json!({}))).await?;ensure!(r["state"]=="admitted","publication reservation lost");}}
    };
    super::lease::release(db, id, &p, &super::lease::Native).await?;
    let receipt = publication.context("publish helper")?;
    verify_receipt(&p, &scope, task, head, tree, &receipt)?;
    if let Some(prior) = j["receipt"].as_str() {
        let prior: Value = serde_json::from_str(prior)?;
        ensure!(
            prior["image"] == receipt["image"],
            "preview artifact changed after recorded publication; reconcile pinned digest"
        );
    }
    check_fresh(db, j, &p, &generation).await?;
    db.conn.execute(
        "UPDATE preview_jobs SET receipt=? WHERE id=?",
        params![receipt.to_string(), id],
    )?;
    phase(db, id, "checkpointing", None)?;
    let checkpoint_root = db.root.clone();
    let checkpoint_task = task.to_owned();
    let checkpoint_head = head.to_owned();
    let checkpoint_main = j["main_head"].as_str().context("main")?.to_owned();
    let image = receipt["image"].as_str().context("image")?.to_owned();
    let checkpoint_id = id.to_owned();
    let validation = p.validation.clone();
    let checkpoint = crate::budget::blocking_timeout(
        std::time::Duration::from_secs(p.timeout_seconds),
        move || {
            let db = Store::open(&checkpoint_root)?;
            crate::run::checkpoint_run(
                &db,
                &checkpoint_task,
                &checkpoint_head,
                &validation,
                Some(&checkpoint_id),
                crate::run::CheckpointOptions {
                    expected_main_head: Some(&checkpoint_main),
                    artifact_digest: Some(&image),
                    build_id: Some(&checkpoint_id),
                },
            )
        },
    )
    .await?;
    ensure!(
        !checkpoint["attestation"].is_null(),
        "preview checkpoint is unsigned"
    );
    check_fresh(db, j, &p, &generation).await?;
    phase(db, id, "publishing_branch", None)?;
    let publish_root = db.root.clone();
    let publish_task = task.to_owned();
    let publish_head = head.to_owned();
    crate::budget::blocking_timeout(Duration::from_secs(p.timeout_seconds), move || {
        crate::run::publish_run(&Store::open(&publish_root)?, &publish_task, &publish_head)
    })
    .await?;
    phase(db, id, "succeeded", None)?;
    Ok(())
}
