//! Operator-owned preview publication, separate from GitHub automatic delivery.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
mod lease;
mod lease_receipts;
pub(crate) use lease_receipts::pending as pending_reservations;
mod feedback;
mod pipeline;
mod process;
mod retry;
mod review;
pub use retry::retry;
#[cfg(test)]
mod tests;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Policy {
    pub project_id: String,
    pub project_slug: String,
    pub enabled: bool,
    pub publisher: PathBuf,
    #[serde(default)]
    pub builder_image: String,
    #[serde(default)]
    pub runtime_image: String,
    #[serde(default)]
    pub dockerfile: String,
    pub validation: Vec<String>,
    pub review_step: String,
    pub admission_url: String,
    pub admission_token_file: PathBuf,
    pub estimated_publish_bytes: u64,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_publish_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_recipe: Option<NativeRecipe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_artifact_store: Option<PathBuf>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeRecipe {
    pub build: Vec<String>,
    pub archive: String,
    pub platform: crate::run::native_artifact::Platform,
    pub entrypoint: Vec<String>,
}
impl NativeRecipe {
    fn validate(&self) -> Result<()> {
        self.platform.validate()?;
        ensure!(
            !self.build.is_empty()
                && self.build.len() <= 32
                && self
                    .build
                    .iter()
                    .all(|arg| !arg.is_empty() && arg.len() <= 4096 && !arg.contains('\0')),
            "invalid native build argv"
        );
        ensure!(
            crate::run::native_artifact::executable(&self.archive),
            "invalid native archive path"
        );
        ensure!(
            !self.entrypoint.is_empty()
                && self.entrypoint.len() <= 32
                && crate::run::native_artifact::executable(&self.entrypoint[0])
                && self
                    .entrypoint
                    .iter()
                    .all(|arg| arg.len() <= 4096 && !arg.contains('\0')),
            "invalid native entrypoint"
        );
        Ok(())
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    scope: String,
    projects: Vec<Policy>,
}
fn image(s: &str) -> bool {
    s.rsplit_once("@sha256:").is_some_and(|(n, h)| {
        !n.is_empty()
            && n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"./_:-".contains(&b))
            && h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn absolute(p: &Path) -> bool {
    p.is_absolute()
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && p.to_str().is_some_and(|s| !s.chars().any(char::is_control))
}
fn configure(v: &Value) -> Result<Config> {
    let c: Config = serde_json::from_value(v.clone()).context("invalid preview pipeline policy")?;
    ensure!(
        c.schema_version == 1 && ["local", "foundry"].contains(&c.scope.as_str()),
        "invalid preview policy scope/version"
    );
    ensure!(c.projects.len() <= 128, "too many preview projects");
    let mut ids = std::collections::BTreeSet::new();
    for p in &c.projects {
        ensure!(
            uuid::Uuid::parse_str(&p.project_id).is_ok() && ids.insert(&p.project_id),
            "invalid or duplicate preview project"
        );
        ensure!(
            !p.project_slug.is_empty()
                && p.project_slug.len() <= 128
                && p.project_slug
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "invalid preview slug"
        );
        ensure!(
            absolute(&p.publisher) && absolute(&p.admission_token_file),
            "preview paths must be absolute without traversal"
        );
        if let Some(recipe) = &p.native_recipe {
            recipe.validate()?;
            ensure!(
                p.native_artifact_store
                    .as_ref()
                    .is_some_and(|path| absolute(path)),
                "native publisher requires an absolute operator-owned artifact store"
            );
            ensure!(
                p.builder_image.is_empty()
                    && p.runtime_image.is_empty()
                    && p.dockerfile.is_empty()
                    && p.docker_host.is_none()
                    && p.registry_publish_endpoint.is_none(),
                "native recipe cannot contain Docker configuration"
            );
        } else {
            ensure!(
                p.native_artifact_store.is_none(),
                "OCI publisher cannot configure native artifact store"
            );
            ensure!(
                image(&p.builder_image) && image(&p.runtime_image),
                "preview images must be pinned by digest"
            );
            ensure!(
                crate::store::scope(&p.dockerfile)? == p.dockerfile && p.dockerfile != ".",
                "invalid preview Dockerfile"
            );
        }
        ensure!(
            !p.validation.is_empty()
                && p.validation.len() <= 32
                && p.validation
                    .iter()
                    .all(|v| !v.is_empty() && v.len() <= 4096 && !v.contains('\0')),
            "invalid preview validation argv"
        );
        ensure!(
            !p.review_step.is_empty()
                && p.review_step.len() <= 128
                && p.review_step
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid review step"
        );
        validate_transport(p)?;
        let url = reqwest::Url::parse(&p.admission_url)?;
        ensure!(
            url.scheme() == "http"
                && (url.host_str() == Some("registry-admission")
                    || (p.native_recipe.is_some()
                        && url.host_str().is_some_and(|host| host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback()))))
                && (url.port() == Some(8092)
                    || (p.native_recipe.is_some() && url.port().is_some_and(|port| port != 0)))
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "admission must be installation-private registry-admission:8092"
        );
        ensure!(
            (1..=7200).contains(&p.timeout_seconds) && p.estimated_publish_bytes > 0,
            "invalid preview bounds"
        );
    }
    Ok(c)
}
fn validate_transport(p: &Policy) -> Result<()> {
    if let Some(host) = &p.docker_host {
        ensure!(
            host == "tcp://sandbox-docker:2375"
                || host
                    .strip_prefix("unix://")
                    .is_some_and(|path| absolute(Path::new(path))),
            "Docker host must be the installation sandbox or an explicit absolute Unix socket"
        );
    }
    if let Some(endpoint) = &p.registry_publish_endpoint {
        let address: std::net::SocketAddr = endpoint
            .parse()
            .context("registry publish endpoint must be a literal loopback address and port")?;
        ensure!(
            address.ip().is_loopback() && address.port() != 0,
            "registry publish endpoint must be loopback with a nonzero port"
        );
        ensure!(
            p.docker_host
                .as_ref()
                .is_some_and(|host| host.starts_with("unix://")),
            "host registry publication requires an explicit Unix Docker host"
        );
    }
    Ok(())
}

impl Policy {
    fn recipe_hash(&self) -> String {
        if let Some(recipe) = &self.native_recipe {
            return crate::store::hash(
                &serde_json::to_vec(
                    &serde_json::to_value(recipe).expect("serializable native recipe"),
                )
                .expect("serializable native recipe"),
            );
        }
        let recipe = format!(
            "{{\"builder_image\": {}, \"component\": {}, \"dockerfile\": {}, \"runtime_image\": {}}}",
            json!(self.builder_image),
            json!(self.component),
            json!(self.dockerfile),
            json!(self.runtime_image)
        );
        let mut ascii = String::new();
        for ch in recipe.chars() {
            if ch.is_ascii() {
                ascii.push(ch)
            } else {
                for unit in ch.encode_utf16(&mut [0; 2]) {
                    ascii.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
        crate::store::hash(ascii.as_bytes())
    }
}
pub fn migrate(db: &Store) -> Result<()> {
    migrate_connection(&db.conn)
}
pub fn migrate_connection(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS preview_policies(project TEXT PRIMARY KEY REFERENCES projects(id),scope TEXT NOT NULL,generation TEXT NOT NULL,policy TEXT NOT NULL,activation_seq INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS preview_review_execution(attempt TEXT PRIMARY KEY,step TEXT NOT NULL,head TEXT NOT NULL,tree TEXT NOT NULL,executor TEXT NOT NULL,workspace TEXT NOT NULL); CREATE TABLE IF NOT EXISTS preview_jobs(id TEXT PRIMARY KEY,task TEXT NOT NULL REFERENCES tasks(id),generation TEXT NOT NULL,head TEXT NOT NULL,tree TEXT NOT NULL,main_head TEXT NOT NULL,recipe TEXT NOT NULL,phase TEXT NOT NULL,receipt TEXT,reservation TEXT,error TEXT,created INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS preview_jobs_task ON preview_jobs(task,created); CREATE TABLE IF NOT EXISTS preview_retries(task TEXT NOT NULL,key TEXT NOT NULL,request TEXT NOT NULL,response TEXT NOT NULL,PRIMARY KEY(task,key)); CREATE TABLE IF NOT EXISTS preview_admissions(job TEXT NOT NULL,ordinal INTEGER NOT NULL,key TEXT NOT NULL,PRIMARY KEY(job,ordinal)); CREATE TABLE IF NOT EXISTS preview_reviews(step TEXT PRIMARY KEY REFERENCES steps(id),task TEXT NOT NULL,head TEXT NOT NULL,tree TEXT NOT NULL,main_head TEXT NOT NULL,generation TEXT NOT NULL,attempt TEXT);")?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS preview_admission_receipts(job TEXT NOT NULL,ordinal INTEGER NOT NULL,reservation TEXT NOT NULL,request TEXT,state TEXT NOT NULL,generation INTEGER NOT NULL,PRIMARY KEY(job,ordinal),FOREIGN KEY(job,ordinal) REFERENCES preview_admissions(job,ordinal));")?;
    Ok(())
}
pub fn setup(db: &Store, v: &Value) -> Result<Value> {
    let c = configure(v)?;
    migrate(db)?;
    let generation = crate::store::hash(&serde_json::to_vec(&c)?);
    db.atomic(|| {
        let activation: i64 = db.conn.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM events", [], |row| row.get(0),
        )?;
        for p in &c.projects {
            crate::projects::resolve(db, &p.project_id)?;
            db.conn.execute(
                "INSERT INTO preview_policies VALUES(?,?,?,?,?) ON CONFLICT(project) DO UPDATE SET scope=excluded.scope,generation=excluded.generation,policy=excluded.policy,activation_seq=CASE WHEN preview_policies.activation_seq<0 AND json_extract(excluded.policy,'$.enabled')=1 THEN excluded.activation_seq ELSE preview_policies.activation_seq END",
                params![p.project_id, c.scope, generation, serde_json::to_string(p)?, if p.enabled { activation } else { -1 }],
            )?;
        }
        Ok(())
    })?;
    Ok(
        json!({"configured":true,"generation":generation,"projects":c.projects.iter().map(|p|json!({"project_id":p.project_id,"enabled":p.enabled})).collect::<Vec<_>>() }),
    )
}
fn policy(db: &Store, project: &str) -> Result<Option<(Policy, String, String)>> {
    migrate(db)?;
    let rows = db.rows(
        "SELECT * FROM preview_policies WHERE project=?",
        &[&project],
    )?;
    rows.first()
        .map(|r| {
            Ok((
                serde_json::from_str(r["policy"].as_str().context("policy")?)?,
                r["generation"].as_str().context("generation")?.into(),
                r["scope"].as_str().context("scope")?.into(),
            ))
        })
        .transpose()
}
pub fn status(db: &Store, task: &str) -> Result<Value> {
    migrate(db)?;
    let project = crate::projects::task_project(db, task)?;
    if policy(db, &project)?.is_none_or(|(p, _, _)| !p.enabled) {
        return Ok(json!({"enabled":false}));
    }
    let jobs=db.rows("SELECT id,head AS commit_sha,tree AS tree_sha,generation,recipe AS recipe_hash,main_head AS expected_main_head,phase,error,receipt FROM preview_jobs WHERE task=? ORDER BY rowid DESC LIMIT 1",&[&task])?;
    let mut v = jobs
        .first()
        .cloned()
        .unwrap_or(json!({"phase":"waiting_for_review"}));
    v["enabled"] = json!(true);
    if let Some(s) = v["receipt"].as_str() {
        v["receipt"] = serde_json::from_str(s)?;
    }
    if let Some(id) = v["id"].as_str() {
        let checkpoints=db.rows("SELECT data FROM events WHERE task=? AND kind='run.checkpoint_verified' AND json_extract(data,'$.idempotency_key')=? ORDER BY seq DESC LIMIT 1",&[&task,&id])?;
        v["checkpoint"] = checkpoints
            .first()
            .and_then(|r| r["data"].as_str())
            .map(serde_json::from_str::<Value>)
            .transpose()?
            .unwrap_or(Value::Null);
        let reviews=db.rows("SELECT r.step,a.id AS attempt FROM preview_reviews r JOIN steps s ON s.id=r.step JOIN attempts a ON a.id=r.attempt AND a.step=s.id WHERE r.task=? AND r.head=? AND r.tree=? AND r.generation=? AND s.state='succeeded' AND a.state='succeeded' ORDER BY a.started DESC LIMIT 1",&[&task,&v["commit_sha"].as_str(),&v["tree_sha"].as_str(),&v["generation"].as_str()])?;
        v["review_step_id"] = reviews
            .first()
            .map(|r| r["step"].clone())
            .unwrap_or(Value::Null);
        v["review_attempt_id"] = reviews
            .first()
            .map(|r| r["attempt"].clone())
            .unwrap_or(Value::Null);
    }
    Ok(v)
}
pub use pipeline::Queue;
#[cfg(test)]
use process::publisher_command;
use process::verify_receipt;
pub(crate) use review::feedback_prompt;
pub use review::{bind_attempt, is_review, prepare_review, verify_execution};
