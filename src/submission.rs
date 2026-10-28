//! Caller receipts deduplicate submissions before live discovery or dispatch.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS submission_receipts(request_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,task TEXT NOT NULL REFERENCES tasks(id),response TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS project_submission_receipts(project TEXT NOT NULL REFERENCES projects(id),request_id TEXT NOT NULL,request_hash TEXT NOT NULL,task TEXT NOT NULL REFERENCES tasks(id),response TEXT NOT NULL,PRIMARY KEY(project,request_id));
CREATE TABLE IF NOT EXISTS project_submission_fences(project TEXT NOT NULL REFERENCES projects(id),request_id TEXT NOT NULL,system_binding_hash TEXT NOT NULL,response TEXT NOT NULL,PRIMARY KEY(project,request_id));
CREATE TRIGGER IF NOT EXISTS submission_fence_immutable BEFORE UPDATE ON project_submission_fences
BEGIN SELECT RAISE(ABORT,'submission fence is immutable'); END;
CREATE TRIGGER IF NOT EXISTS submission_fence_delete_immutable BEFORE DELETE ON project_submission_fences
BEGIN SELECT RAISE(ABORT,'submission fence is immutable'); END;
CREATE TRIGGER IF NOT EXISTS submission_fence_positive_guard BEFORE INSERT ON project_submission_fences
WHEN EXISTS(SELECT 1 FROM project_submission_receipts WHERE project=NEW.project AND request_id=NEW.request_id)
 OR (NEW.project='default' AND EXISTS(SELECT 1 FROM submission_receipts WHERE request_id=NEW.request_id))
BEGIN SELECT RAISE(ABORT,'submission already admitted'); END;
CREATE TRIGGER IF NOT EXISTS project_submission_fence_insert_guard BEFORE INSERT ON project_submission_receipts
WHEN EXISTS(SELECT 1 FROM project_submission_fences WHERE project=NEW.project AND request_id=NEW.request_id)
BEGIN SELECT RAISE(ABORT,'submission request is terminally fenced as not admitted'); END;
CREATE TRIGGER IF NOT EXISTS project_submission_fence_update_guard BEFORE UPDATE ON project_submission_receipts
WHEN EXISTS(SELECT 1 FROM project_submission_fences WHERE project=NEW.project AND request_id=NEW.request_id)
BEGIN SELECT RAISE(ABORT,'submission request is terminally fenced as not admitted'); END;
CREATE TRIGGER IF NOT EXISTS legacy_submission_fence_insert_guard BEFORE INSERT ON submission_receipts
WHEN EXISTS(SELECT 1 FROM project_submission_fences WHERE project='default' AND request_id=NEW.request_id)
BEGIN SELECT RAISE(ABORT,'submission request is terminally fenced as not admitted'); END;
CREATE TRIGGER IF NOT EXISTS legacy_submission_fence_update_guard BEFORE UPDATE ON submission_receipts
WHEN EXISTS(SELECT 1 FROM project_submission_fences WHERE project='default' AND request_id=NEW.request_id)
BEGIN SELECT RAISE(ABORT,'submission request is terminally fenced as not admitted'); END;")?;
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SystemBinding {
    organization_id: String,
    project_id: String,
    tenant_id: String,
    thread_id: String,
    original_actor_id: String,
    operation_id: String,
    action: String,
    request_hash: String,
    objective_sha256: String,
    invocation_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRequest {
    schema_version: u32,
    project: String,
    request_id: String,
    system_binding: SystemBinding,
    repair_actor_id: String,
    continuity_evidence_sha256: String,
}

fn canonical_uuid(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.to_string() == value,
        "canonical UUID required"
    );
    Ok(())
}
fn sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
impl CloseRequest {
    fn validate(&self, db: &Store) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported submission settlement version"
        );
        let binding = &self.system_binding;
        for value in [
            &binding.organization_id,
            &binding.project_id,
            &binding.thread_id,
            &binding.original_actor_id,
            &binding.invocation_id,
            &self.repair_actor_id,
        ] {
            canonical_uuid(value)?;
        }
        ensure!(
            binding.action == "build",
            "only an original Build can be fenced"
        );
        ensure!(
            !binding.operation_id.is_empty()
                && binding.operation_id.len() <= 128
                && binding
                    .operation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b":-_.".contains(&byte)),
            "bounded original operation ID required"
        );
        ensure!(
            sha256_hex(&binding.request_hash)
                && sha256_hex(&binding.objective_sha256)
                && sha256_hex(&self.continuity_evidence_sha256),
            "SHA256 audit/binding fields required"
        );
        ensure!(
            crate::projects::tenant(db, &self.project)? == binding.tenant_id,
            "submission settlement tenant scope mismatch"
        );
        let key = crate::store::hash(
            format!("{}:{}", binding.original_actor_id, binding.operation_id).as_bytes(),
        );
        ensure!(
            self.request_id == format!("foundry:{key}"),
            "original actor/operation request key mismatch"
        );
        Ok(())
    }
    fn binding_hash(&self) -> String {
        let binding = &self.system_binding;
        let fields = BTreeMap::from([
            ("action", binding.action.as_str()),
            ("invocation_id", binding.invocation_id.as_str()),
            ("objective_sha256", binding.objective_sha256.as_str()),
            ("operation_id", binding.operation_id.as_str()),
            ("organization_id", binding.organization_id.as_str()),
            ("original_actor_id", binding.original_actor_id.as_str()),
            ("project_id", binding.project_id.as_str()),
            ("request_hash", binding.request_hash.as_str()),
            ("tenant_id", binding.tenant_id.as_str()),
            ("thread_id", binding.thread_id.as_str()),
        ]);
        let object = fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), Value::String(value.to_owned())))
            .collect();
        crate::store::hash(Value::Object(object).to_string().as_bytes())
    }
}

fn fenced(db: &Store, project: &str, request: &str) -> Result<bool> {
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_submission_fences WHERE project=? AND request_id=?)",
        [project, request],
        |row| row.get(0),
    )?)
}

fn admitted_receipt(db: &Store, project: &str, request: &str) -> Result<Option<Value>> {
    let rows = db.rows("SELECT task,response,0 AS legacy FROM project_submission_receipts WHERE project=? AND request_id=? UNION ALL SELECT task,response,1 AS legacy FROM submission_receipts WHERE request_id=? AND ?='default' ORDER BY legacy LIMIT 1",
        &[&project, &request, &request, &project])?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let task = row["task"].as_str().context("durable submitted task")?;
    crate::projects::authorize_task(db, project, task)?;
    let receipt: Value =
        serde_json::from_str(row["response"].as_str().context("submission receipt")?)?;
    ensure!(
        receipt["id"] == task,
        "durable submission identity mismatch"
    );
    ensure!(
        !fenced(db, project, request)?,
        "contradictory submission receipts require reconciliation"
    );
    Ok(Some(
        json!({"schema_version":1,"state":"admitted","admission_fenced":false,
        "project":project,"request_id":request,"task_id":task,"receipt":receipt}),
    ))
}

/// Permanently prevent one unadmitted stable Build request without dispatching it.
///
/// This private operator/service operation is not a worker or model capability.
/// The configured installation's continuity audit is an observation, not authority.
///
/// # Errors
/// Rejects scope/identity changes, conflicting fences, invalid input and storage uncertainty.
pub fn close_unadmitted(db: &Store, args: &Value) -> Result<Value> {
    let request: CloseRequest = serde_json::from_value(args.clone())?;
    request.validate(db)?;
    let binding_hash = request.binding_hash();
    db.atomic(|| {
        if let Some(receipt) = admitted_receipt(db, &request.project, &request.request_id)? {
            return Ok(receipt);
        }
        let prior = db.rows("SELECT system_binding_hash,response FROM project_submission_fences WHERE project=? AND request_id=?",
            &[&request.project, &request.request_id])?;
        if let Some(row) = prior.first() {
            ensure!(row["system_binding_hash"] == binding_hash, "submission fence already binds another original request");
            let receipt: Value = serde_json::from_str(row["response"].as_str().context("terminal submission receipt")?)?;
            ensure!(receipt["state"] == "not_admitted" && receipt["admission_fenced"] == true
                && receipt.get("task_id").is_some_and(Value::is_null)
                && receipt["project"] == request.project && receipt["request_id"] == request.request_id
                && receipt["system_binding_hash"] == binding_hash, "terminal submission receipt identity mismatch");
            return Ok(receipt);
        }
        let receipt = json!({"schema_version":1,"state":"not_admitted","admission_fenced":true,
            "project":request.project,"request_id":request.request_id,"task_id":null,
            "system_binding_hash":binding_hash,"receipt_id":crate::store::id(),"created_at":crate::store::now(),
            "repair_actor_id":request.repair_actor_id,"continuity_evidence_sha256":request.continuity_evidence_sha256});
        db.conn.execute("INSERT INTO project_submission_fences(project,request_id,system_binding_hash,response) VALUES(?,?,?,?)",
            rusqlite::params![request.project, request.request_id, binding_hash, receipt.to_string()])?;
        Ok(receipt)
    })
}

/// Find a matching historical submission before touching its original checkout.
pub fn receipt_project(db: &Store, args: &Value) -> Result<Option<String>> {
    let Some(request) = args["request_id"].as_str() else {
        return Ok(None);
    };
    let candidates = db.rows("SELECT project,request_hash,0 AS legacy FROM project_submission_receipts WHERE request_id=? UNION ALL SELECT 'default' AS project,request_hash,1 AS legacy FROM submission_receipts WHERE request_id=?", &[&request, &request])?;
    let projects: std::collections::BTreeSet<String> = candidates
        .iter()
        .filter_map(|row| row["project"].as_str().map(str::to_owned))
        .collect();
    let mut matched: Option<String> = None;
    for row in candidates {
        let project = row["project"].as_str().context("receipt project")?;
        let mut input = args.clone();
        if row["legacy"] == 1 {
            input
                .as_object_mut()
                .context("submission arguments")?
                .remove("project");
        } else {
            input["project"] = serde_json::json!(project);
        }
        let hash = crate::store::hash(&serde_json::to_vec(&input)?);
        if row["request_hash"] == hash {
            ensure!(
                matched.as_deref().is_none_or(|old| old == project),
                "submission receipt is ambiguous; select a project explicitly"
            );
            matched = Some(project.to_owned());
        }
    }
    // With a unique receipt owner, reject changed retries using the durable
    // assignment even if its original checkout no longer exists.
    Ok(matched.or_else(|| {
        (projects.len() == 1)
            .then(|| projects.into_iter().next())
            .flatten()
    }))
}

pub fn existing(db: &Store, args: &Value) -> Result<Option<Value>> {
    let Some(request) = args["request_id"].as_str() else {
        return Ok(None);
    };
    let project = args["project"].as_str().context("submission project")?;
    ensure!(
        !fenced(db, project, request)?,
        "submission request is terminally fenced as not admitted"
    );
    let mut legacy_args = args.clone();
    legacy_args
        .as_object_mut()
        .context("submission arguments")?
        .remove("project");
    let legacy_hash = crate::store::hash(&serde_json::to_vec(&legacy_args)?);
    let hash = crate::store::hash(&serde_json::to_vec(args)?);
    let rows = db.rows("SELECT request_hash,response,0 AS legacy FROM project_submission_receipts WHERE project=? AND request_id=? UNION ALL SELECT request_hash,response,1 AS legacy FROM submission_receipts WHERE request_id=? AND ?='default' ORDER BY legacy LIMIT 1", &[&project,&request,&request,&project])?;
    let Some(receipt) = rows.first() else {
        return Ok(None);
    };
    ensure!(
        receipt["request_hash"] == hash
            || (receipt["legacy"] == 1 && receipt["request_hash"] == legacy_hash),
        "submission request_id reused with different assignment"
    );
    let response: Value =
        serde_json::from_str(receipt["response"].as_str().context("submission receipt")?)?;
    crate::projects::authorize_task(
        db,
        project,
        response["id"].as_str().context("receipt task")?,
    )?;
    Ok(Some(response))
}

pub fn submit(db: &Store, args: &Value) -> Result<Value> {
    let request = args["request_id"]
        .as_str()
        .context("request_id must be a string")?;
    ensure!(
        !request.trim().is_empty() && request.len() <= 256,
        "request_id must contain 1..256 bytes"
    );
    let project = crate::projects::resolve(
        db,
        args["project"]
            .as_str()
            .unwrap_or(crate::projects::DEFAULT_PROJECT),
    )?;
    let mut canonical = args.clone();
    canonical["project"] = serde_json::json!(project);
    let hash = crate::store::hash(&serde_json::to_vec(&canonical)?);
    db.atomic(|| {
        if let Some(receipt) = existing(db, &canonical)? {
            return Ok(receipt);
        }
        let mut input = canonical.clone();
        input
            .as_object_mut()
            .context("submission arguments")?
            .remove("request_id");
        let response = crate::protocol::dispatch(db, "submit_task", input, None)?;
        db.conn.execute(
            "INSERT INTO project_submission_receipts VALUES(?,?,?,?,?)",
            rusqlite::params![
                project,
                request,
                hash,
                response["id"].as_str().context("submitted task")?,
                response.to_string()
            ],
        )?;
        Ok(response)
    })
}

// These are formatter-domain String vectors, not validated settlement requests.
// Public admission/tenant validation is unchanged and is not invoked here.
#[cfg(test)]
mod binding_hash_wire_compatibility {
    use super::*;

    #[test]
    fn canonical_string_hash_preserves_unicode_escaping_and_sorted_wire_bytes() {
        for (tenant, expected_digest) in [
            (
                "default",
                "0f8c26a4693d6bfe0338f210d8fba0a9194465c08bda556e8bb8c39f1ad65022",
            ),
            (
                "quotes\"and\\slash/",
                "5711e7098167b4aa061b051d482dc843297ef11c998d70392b54adbdc778b22a",
            ),
            (
                "café 🦀 漢字",
                "e58c82cb1eb239c864dc8d5088fd0dae0be37bf93689323d188ece3372b1e768",
            ),
            (
                "line\nreturn\rtab\tbackspace\u{0008}formfeed\u{000c}nul\0",
                "90b132d459d1fd14ce8dd89298b90c6aee18867812f55f5658a44bec4547e2e3",
            ),
            (
                "separator\u{2028}\u{2029}",
                "904b76bdb83b370026d65ab2c039315dd760d7671001484dccdd6459e55f3d9a",
            ),
        ] {
            let request: CloseRequest = serde_json::from_value(json!({
                "schema_version":1,"project":"default","request_id":"formatter-only",
                "system_binding":{
                    "organization_id":"00000000-0000-4000-8000-000000000003",
                    "project_id":"00000000-0000-4000-8000-000000000004",
                    "tenant_id":tenant,"thread_id":"00000000-0000-4000-8000-000000000005",
                    "original_actor_id":"00000000-0000-4000-8000-000000000001",
                    "operation_id":"op-fixture-build","action":"build",
                    "request_hash":crate::store::hash(b"original System reservation"),
                    "objective_sha256":crate::store::hash(b"frozen fixture objective"),
                    "invocation_id":"00000000-0000-4000-8000-000000000006"
                },
                "repair_actor_id":"00000000-0000-4000-8000-000000000002",
                "continuity_evidence_sha256":crate::store::hash(b"formatter-only")
            }))
            .expect("exact private String field types");
            let legacy_fields: BTreeMap<String, Value> = serde_json::from_value(
                serde_json::to_value(&request.system_binding).expect("legacy String serialization"),
            )
            .expect("legacy String-keyed Value object");
            assert_eq!(legacy_fields.len(), 10);
            let legacy_bytes = serde_json::to_vec(&legacy_fields).expect("legacy Vec serializer");
            assert_eq!(crate::store::hash(&legacy_bytes), expected_digest);
            // Baseline API returns Result<String>; after the proposed infallible
            // signature change, remove only .expect(...), retaining all assertions.
            let actual = request.binding_hash();
            assert_eq!(actual, expected_digest);
            assert_eq!(actual, crate::store::hash(&legacy_bytes));
        }
    }
}
