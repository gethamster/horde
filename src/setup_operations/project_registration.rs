//! Strict, DB-only registration of existing operator-owned local checkouts.
pub(crate) mod strict;
use crate::{projects, store::Store};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug)]
pub(crate) struct Conflict(pub &'static str);
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Conflict {}
fn require(ok: bool, code: &'static str) -> Result<()> {
    if !ok {
        return Err(Conflict(code).into());
    }
    Ok(())
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    schema_version: u32,
    scope: String,
    projects: Vec<Project>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Project {
    id: String,
    slug: String,
    name: String,
    tenant_id: String,
    concurrency: u32,
    isolation: String,
    repository_slug: String,
    runtime: String,
}
fn slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        && uuid::Uuid::parse_str(s).is_err()
}
pub(crate) fn validate(value: &Value) -> Result<Config> {
    let config: Config = serde_json::from_value(value.clone())?;
    ensure!(
        config.schema_version == 1 && config.scope == "foundry",
        "invalid version or scope"
    );
    ensure!((1..=100).contains(&config.projects.len()), "invalid count");
    let mut ids = BTreeSet::new();
    let mut slugs = BTreeSet::new();
    let mut repos = BTreeSet::new();
    for p in &config.projects {
        ensure!(
            uuid::Uuid::parse_str(&p.id)?.to_string() == p.id,
            "invalid UUID"
        );
        ensure!(slug(&p.slug) && slug(&p.repository_slug), "invalid slug");
        ensure!(
            !p.name.trim().is_empty()
                && p.name.len() <= 256
                && !p.name.chars().any(char::is_control),
            "invalid name"
        );
        ensure!(
            p.tenant_id == "foundry"
                && p.runtime == "local"
                && (1..=64).contains(&p.concurrency)
                && matches!(p.isolation.as_str(), "native" | "vm"),
            "invalid configuration"
        );
        require(
            ids.insert(&p.id) && slugs.insert(&p.slug) && repos.insert(&p.repository_slug),
            "duplicate_identity",
        )?;
    }
    Ok(config)
}
#[derive(Serialize, PartialEq, Eq)]
struct Repository {
    path: PathBuf,
    common: PathBuf,
    checkout_device: u64,
    checkout_inode: u64,
    metadata_device: u64,
    metadata_inode: u64,
}
#[derive(Serialize)]
pub(crate) struct ContextIdentity {
    root: PathBuf,
    repositories: Vec<Repository>,
    runtime: String,
}
impl ContextIdentity {
    pub(crate) fn fingerprint(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(self)?)))
    }
}
fn ordinary_ancestors(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "absolute operator root required");
    for ancestor in path.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "ordinary directory required"
        );
    }
    Ok(())
}
fn inspect(root: &Path, slug: &str) -> Result<Repository> {
    ordinary_ancestors(root)?;
    let path = root.join(slug);
    ordinary_ancestors(&path)?;
    ordinary_ancestors(&path.join(".git"))?;
    // Git indirection files and metadata links are not part of this boundary.
    for name in ["commondir", "gitdir"] {
        ensure!(
            !path.join(".git").join(name).try_exists()?,
            "indirect metadata"
        );
    }
    let mut todo = vec![path.join(".git")];
    let mut count = 0;
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(dir)? {
            count += 1;
            ensure!(count <= 100_000, "metadata bound exceeded");
            let entry = entry?;
            let ty = entry.file_type()?;
            ensure!(
                !ty.is_symlink() && (ty.is_dir() || ty.is_file()),
                "indirect metadata"
            );
            if ty.is_dir() {
                todo.push(entry.path());
            }
        }
    }
    let mut command = std::process::Command::new("git");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(&path)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
            "--is-bare-repository",
        ]);
    let output = crate::budget::command_output_with_timeout(&mut command, Duration::from_secs(3))?;
    ensure!(
        output.status.success() && output.stdout.len() <= 16384,
        "invalid Git checkout"
    );
    let text = std::str::from_utf8(&output.stdout)?;
    let lines: Vec<_> = text.lines().collect();
    let path = path.canonicalize()?;
    let common = path.join(".git");
    ensure!(
        lines.len() == 3
            && Path::new(lines[0]) == path
            && Path::new(lines[1]) == common
            && lines[2] == "false",
        "Git identity mismatch"
    );
    use std::os::unix::fs::MetadataExt;
    let checkout_metadata = std::fs::metadata(&path)?;
    let git_metadata = std::fs::metadata(&common)?;
    Ok(Repository {
        path,
        common,
        checkout_device: checkout_metadata.dev(),
        checkout_inode: checkout_metadata.ino(),
        metadata_device: git_metadata.dev(),
        metadata_inode: git_metadata.ino(),
    })
}
pub(crate) fn context(db: &Store, root: &Path, config: &Config) -> Result<ContextIdentity> {
    let repositories = config
        .projects
        .iter()
        .map(|p| inspect(root, &p.repository_slug))
        .collect::<Result<Vec<_>>>()
        .map_err(|_| Conflict("repository_invalid"))?;
    require(
        repositories
            .iter()
            .map(|r| (r.metadata_device, r.metadata_inode))
            .collect::<BTreeSet<_>>()
            .len()
            == repositories.len(),
        "duplicate_identity",
    )?;
    Ok(ContextIdentity {
        root: root
            .canonicalize()
            .map_err(|_| Conflict("repository_invalid"))?,
        repositories,
        runtime: projects::local_runtime_identity(db)?,
    })
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrationResult {
    schema_version: u32,
    scope: String,
    projects: Vec<RegisteredProject>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisteredProject {
    id: String,
    slug: String,
    name: String,
    tenant_id: String,
    concurrency: u32,
    isolation: String,
    repository: RegisteredRepository,
    runtime: String,
    disposition: Disposition,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisteredRepository {
    id: String,
    slug: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Disposition {
    Created,
    Reconciled,
}
struct Binding {
    exists: bool,
    repository: Option<String>,
    granted: bool,
}
fn same_physical_directory(path: &Path, device: u64, inode: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_dir() && m.dev() == device && m.ino() == inode)
}
fn binding(db: &Store, p: &Project, repo: &Repository, local: &str) -> Result<Binding> {
    let rows = db.rows(
        "SELECT * FROM projects WHERE id=? OR slug=?",
        &[&p.id, &p.slug],
    )?;
    require(rows.len() <= 1, "project_conflict")?;
    let exists = !rows.is_empty();
    if let Some(row) = rows.first() {
        require(
            row["id"] == p.id
                && row["slug"] == p.slug
                && row["name"] == p.name
                && row["concurrency"] == p.concurrency
                && row["isolation"] == p.isolation
                && projects::tenant(db, &p.id)? == p.tenant_id,
            "project_conflict",
        )?;
    }
    let mut repository = None;
    // Compare physical identities too: old native registrations may retain an alias path.
    for row in db.rows("SELECT * FROM project_repositories", &[])? {
        let stored_path = row["path"].as_str().context("repository path")?;
        let stored_common = row["common_dir"].as_str().context("repository common")?;
        let same_path = same_physical_directory(
            Path::new(stored_path),
            repo.checkout_device,
            repo.checkout_inode,
        ) || Path::new(stored_path) == repo.path
            || Path::new(stored_path)
                .canonicalize()
                .is_ok_and(|v| v == repo.path);
        let same_common = same_physical_directory(
            Path::new(stored_common),
            repo.metadata_device,
            repo.metadata_inode,
        ) || Path::new(stored_common) == repo.common
            || Path::new(stored_common)
                .canonicalize()
                .is_ok_and(|v| v == repo.common);
        if same_path || same_common {
            require(
                row["project"] == p.id
                    && Path::new(stored_path) == repo.path
                    && Path::new(stored_common) == repo.common
                    && repository.is_none(),
                "repository_conflict",
            )?;
            let id = row["id"].as_str().context("repository id")?;
            require(
                uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id),
                "repository_conflict",
            )?;
            repository = Some(id.to_owned());
        }
    }
    let revoked: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM project_runtime_revocations WHERE project=? AND runtime IN ('local',?))", params![p.id, local], |r| r.get(0))?;
    let dedicated: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM runtime_project_bindings WHERE runtime IN ('local',?) AND project!=?)", params![local, p.id], |r| r.get(0))?;
    require(!revoked && !dedicated, "runtime_conflict")?;
    let granted: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM project_runtime_grants WHERE project=? AND runtime IN ('local',?))", params![p.id, local], |r| r.get(0))?;
    let occupied: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_projects WHERE project=?)",
        [&p.id],
        |r| r.get(0),
    )?;
    require(
        !occupied || (exists && repository.is_some() && granted),
        "project_has_history",
    )?;
    Ok(Binding {
        exists,
        repository,
        granted,
    })
}
pub(crate) fn quiet(db: &Store) -> Result<()> {
    super::quiet(db).map_err(|e| {
        if e.downcast_ref::<super::RuntimeNotQuiet>().is_some() {
            Conflict("runtime_not_quiescent").into()
        } else {
            e
        }
    })
}
pub(crate) fn apply(db: &Store, root: &Path, config: &Config, expected: &str) -> Result<Value> {
    quiet(db)?;
    let context = context(db, root, config)?;
    require(context.fingerprint()? == expected, "context_conflict")?;
    let bindings = config
        .projects
        .iter()
        .zip(&context.repositories)
        .map(|(p, r)| binding(db, p, r, &context.runtime))
        .collect::<Result<Vec<_>>>()?;
    let mut results = Vec::new();
    for ((p, repo), binding) in config
        .projects
        .iter()
        .zip(&context.repositories)
        .zip(bindings)
    {
        if !binding.exists {
            projects::dispatch(db, "project_create", &serde_json::to_value(p)?)?;
        }
        let repository = match binding.repository {
            Some(id) => id,
            None => projects::register_repository_identity(db, &p.id, &repo.path, &repo.common)?,
        };
        if !binding.granted {
            // Revocation and dedication were checked under the same immediate transaction.
            // Native grant logic retains its own checks; incidental identity cache is transactional.
            require(
                projects::local_runtime_identity(db)? == context.runtime,
                "context_conflict",
            )?;
            projects::dispatch(
                db,
                "project_runtime_grant",
                &json!({"project":p.id,"runtime":"local"}),
            )?;
        }
        results.push(RegisteredProject {
            id: p.id.clone(),
            slug: p.slug.clone(),
            name: p.name.clone(),
            tenant_id: p.tenant_id.clone(),
            concurrency: p.concurrency,
            isolation: p.isolation.clone(),
            repository: RegisteredRepository {
                id: repository,
                slug: p.repository_slug.clone(),
            },
            runtime: "local".into(),
            disposition: if binding.exists {
                Disposition::Reconciled
            } else {
                Disposition::Created
            },
        });
    }
    Ok(serde_json::to_value(RegistrationResult {
        schema_version: 1,
        scope: "foundry".into(),
        projects: results,
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn payload() -> Value {
        json!({"schema_version":1,"scope":"foundry","projects":[{"id":"11111111-1111-4111-8111-111111111111","slug":"deliver","name":"Deliver","tenant_id":"foundry","concurrency":1,"isolation":"native","repository_slug":"deliver","runtime":"local"}]})
    }
    #[test]
    fn typed_bounds_required_fields_and_no_coercion() {
        let original = payload();
        for field in ["schema_version", "scope", "projects"] {
            let mut value = original.clone();
            value.as_object_mut().unwrap().remove(field);
            assert!(validate(&value).is_err(), "missing {field}");
        }
        for field in [
            "id",
            "slug",
            "name",
            "tenant_id",
            "concurrency",
            "isolation",
            "repository_slug",
            "runtime",
        ] {
            let mut value = original.clone();
            value["projects"][0].as_object_mut().unwrap().remove(field);
            assert!(validate(&value).is_err(), "missing {field}");
            let mut value = original.clone();
            value["projects"][0][field] = Value::Null;
            assert!(validate(&value).is_err(), "null {field}");
        }
        for (field, value) in [
            ("schema_version", json!(1.0)),
            ("schema_version", json!(2)),
            ("schema_version", json!("1")),
            ("scope", json!("local")),
            ("unknown", json!(0)),
            ("projects", json!([])),
        ] {
            let mut invalid = original.clone();
            invalid[field] = value;
            assert!(validate(&invalid).is_err(), "{field}");
        }
        for (field, value) in [
            ("concurrency", json!(0)),
            ("concurrency", json!(65)),
            ("concurrency", json!(1.0)),
            ("concurrency", json!("1")),
            ("name", json!(" ")),
            ("name", json!("é".repeat(129))),
            ("name", json!("a\u{007f}")),
            ("slug", json!("UPPER")),
            ("slug", json!("a".repeat(65))),
            ("slug", json!("11111111-1111-4111-8111-111111111111")),
            ("isolation", json!("remote")),
            ("runtime", json!("local-id")),
            ("tenant_id", json!("other")),
        ] {
            let mut invalid = original.clone();
            invalid["projects"][0][field] = value;
            assert!(validate(&invalid).is_err(), "{field}");
        }
        let mut valid = original.clone();
        valid["projects"][0]["concurrency"] = json!(64);
        valid["projects"][0]["isolation"] = json!("vm");
        valid["projects"][0]["name"] = json!("é".repeat(128));
        valid["projects"][0]["slug"] = json!("a".repeat(64));
        assert!(validate(&valid).is_ok());
        let projects = (1..=100)
            .map(|i| {
                let mut p = original["projects"][0].clone();
                p["id"] = json!(format!("{i:08x}-1111-4111-8111-111111111111"));
                p["slug"] = json!(format!("p{i}"));
                p["repository_slug"] = json!(format!("r{i}"));
                p
            })
            .collect::<Vec<_>>();
        valid["projects"] = json!(projects);
        assert!(validate(&valid).is_ok());
        valid["projects"]
            .as_array_mut()
            .unwrap()
            .push(original["projects"][0].clone());
        assert!(validate(&valid).is_err());
    }
    #[test]
    fn strict_json_rejects_nested_escaped_duplicates_and_retains_numbers() {
        for value in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":{"name":"first","na\u006de":"second"}}"#,
            r#"[{"x":null,"x":false}]"#,
        ] {
            assert!(strict::decode(value).is_err());
        }
        let decoded = strict::decode(r#"{"array":[null,false,-1,1,1.0,"text"]}"#).unwrap();
        assert!(decoded["array"][3].is_u64());
        assert!(decoded["array"][4].is_f64());
    }
}
