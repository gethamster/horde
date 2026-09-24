//! Authoritative project ownership, repository registration, and runtime grants.
use crate::store::{Store, id, now};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const DEFAULT_PROJECT: &str = "default";

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("SAVEPOINT project_migration")?;
    let result = (|| -> Result<()> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY,slug TEXT NOT NULL UNIQUE,name TEXT NOT NULL,concurrency INTEGER NOT NULL CHECK(concurrency BETWEEN 1 AND 64),isolation TEXT NOT NULL CHECK(isolation IN ('native','vm')),created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS project_repositories(id TEXT PRIMARY KEY,project TEXT NOT NULL REFERENCES projects(id),path TEXT NOT NULL,common_dir TEXT NOT NULL UNIQUE,UNIQUE(id,project));
CREATE TABLE IF NOT EXISTS task_projects(task TEXT PRIMARY KEY REFERENCES tasks(id),project TEXT NOT NULL REFERENCES projects(id),repository TEXT,FOREIGN KEY(repository,project) REFERENCES project_repositories(id,project));
CREATE INDEX IF NOT EXISTS task_projects_owner ON task_projects(project,task);
CREATE TABLE IF NOT EXISTS project_runtime_grants(project TEXT NOT NULL REFERENCES projects(id),runtime TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(project,runtime));
CREATE TABLE IF NOT EXISTS project_runtime_revocations(project TEXT NOT NULL REFERENCES projects(id),runtime TEXT NOT NULL,PRIMARY KEY(project,runtime));
CREATE TABLE IF NOT EXISTS runtime_project_bindings(runtime TEXT PRIMARY KEY,project TEXT NOT NULL REFERENCES projects(id));
CREATE TABLE IF NOT EXISTS project_tenants(project TEXT PRIMARY KEY REFERENCES projects(id),tenant_id TEXT NOT NULL,explicit INTEGER NOT NULL CHECK(explicit IN (0,1)));
CREATE TRIGGER IF NOT EXISTS runtime_project_immutable BEFORE UPDATE ON runtime_project_bindings BEGIN SELECT RAISE(ABORT,'runtime project ownership is immutable'); END;
INSERT OR IGNORE INTO projects VALUES('default','default','Default',64,'native',0);
INSERT OR IGNORE INTO project_tenants SELECT id,id,0 FROM projects;
CREATE TRIGGER IF NOT EXISTS task_project_immutable BEFORE UPDATE ON task_projects BEGIN SELECT RAISE(ABORT,'task project and repository ownership is immutable'); END;
CREATE TRIGGER IF NOT EXISTS repository_project_immutable BEFORE UPDATE OF project,common_dir ON project_repositories BEGIN SELECT RAISE(ABORT,'repository ownership is immutable'); END;
CREATE TRIGGER IF NOT EXISTS project_identity_immutable BEFORE UPDATE OF id ON projects BEGIN SELECT RAISE(ABORT,'project identity is immutable'); END;
CREATE TRIGGER IF NOT EXISTS project_tenant_immutable BEFORE UPDATE ON project_tenants
WHEN OLD.explicit=1 OR EXISTS(SELECT 1 FROM task_projects WHERE project=OLD.project)
BEGIN SELECT RAISE(ABORT,'project tenant binding is immutable'); END;
CREATE TRIGGER IF NOT EXISTS project_tenant_delete_immutable BEFORE DELETE ON project_tenants
BEGIN SELECT RAISE(ABORT,'project tenant binding is immutable'); END;
")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 6 {
            conn.execute("INSERT OR IGNORE INTO project_runtime_grants SELECT 'default',id,created FROM managed_runtimes", [])?;
        }
        let legacy = conn
            .prepare("SELECT t.id,t.repo,COALESCE(root.repo,t.repo) FROM tasks t LEFT JOIN task_tree tree ON tree.task=t.id LEFT JOIN tasks root ON root.id=tree.root WHERE t.id NOT IN (SELECT task FROM task_projects)")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?,r.get::<_,String>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (task, physical_repo, logical_repo) in legacy {
            migrate_repository(conn, &physical_repo)?;
            let repository = migrate_repository(conn, &logical_repo)?;
            conn.execute(
                "INSERT INTO task_projects VALUES(?,'default',?)",
                params![task, repository],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("RELEASE project_migration")?;
            Ok(())
        }
        Err(error) => {
            conn.execute_batch("ROLLBACK TO project_migration; RELEASE project_migration")?;
            Err(error)
        }
    }
}

fn migrate_repository(conn: &Connection, repo: &str) -> Result<String> {
    let common = common_dir(Path::new(repo)).unwrap_or_else(|_| PathBuf::from(repo));
    let repository = conn
        .query_row(
            "SELECT id FROM project_repositories WHERE common_dir=? AND project='default'",
            [common.to_string_lossy().as_ref()],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    if let Some(repository) = repository {
        return Ok(repository);
    }
    let repository = id();
    conn.execute(
        "INSERT INTO project_repositories VALUES(?,'default',?,?)",
        params![repository, repo, common.to_string_lossy()],
    )?;
    Ok(repository)
}

pub fn resolve(db: &Store, project: &str) -> Result<String> {
    db.conn
        .query_row(
            "SELECT id FROM projects WHERE id=? OR slug=?",
            params![project, project],
            |r| r.get(0),
        )
        .optional()?
        .context("unknown project")
}
pub fn storage_root(db: &Store, project: &str) -> Result<PathBuf> {
    let project = resolve(db, project)?;
    Ok(if project == DEFAULT_PROJECT {
        db.root.clone()
    } else {
        db.root.join("projects").join(project)
    })
}
pub fn task_project(db: &Store, task: &str) -> Result<String> {
    db.conn
        .query_row(
            "SELECT project FROM task_projects WHERE task=?",
            [task],
            |r| r.get(0),
        )
        .optional()?
        .context("task project not found")
}
/// Operator-owned tenant binding. Existing installations retain one tenant per
/// project until an unused project is explicitly assigned to a shared tenant.
pub fn tenant(db: &Store, project: &str) -> Result<String> {
    let project = resolve(db, project)?;
    Ok(db
        .conn
        .query_row(
            "SELECT tenant_id FROM project_tenants WHERE project=?",
            [&project],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(project))
}
fn validate_tenant(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
        "tenant_id must contain 1..128 letters, digits, hyphens or underscores"
    );
    Ok(())
}
pub fn authorize_task(db: &Store, project: &str, task: &str) -> Result<()> {
    ensure!(
        task_project(db, task).ok().as_deref() == Some(resolve(db, project)?.as_str()),
        "task not found in project"
    );
    Ok(())
}

fn common_dir(repo: &Path) -> Result<PathBuf> {
    let repo = repo
        .canonicalize()
        .context("repository directory does not exist")?;
    ensure!(repo.is_dir(), "repository must be a directory");
    let output = crate::executor::clean_command("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()?;
    if output.status.success() {
        return PathBuf::from(String::from_utf8(output.stdout)?.trim())
            .canonicalize()
            .context("Git common directory");
    }
    // Native workflows may use a plain directory, which still has one owner.
    Ok(repo)
}
pub fn infer(db: &Store, repo: &Path) -> Result<Option<String>> {
    let common = common_dir(repo)?;
    Ok(db
        .conn
        .query_row(
            "SELECT project FROM project_repositories WHERE common_dir=?",
            [common.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .optional()?)
}
pub fn register_repository(db: &Store, project: &str, repo: &Path) -> Result<String> {
    let project = resolve(db, project)?;
    let common = common_dir(repo)?;
    let existing: Option<(String, String)> = db
        .conn
        .query_row(
            "SELECT id,project FROM project_repositories WHERE common_dir=?",
            [common.to_string_lossy().as_ref()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((id, owner)) = existing {
        ensure!(
            owner == project,
            "repository or its worktrees already belong to another project"
        );
        return Ok(id);
    }
    let id = id();
    db.conn.execute(
        "INSERT INTO project_repositories VALUES(?,?,?,?)",
        params![
            id,
            project,
            repo.canonicalize()?.to_string_lossy(),
            common.to_string_lossy()
        ],
    )?;
    Ok(id)
}
pub fn bind_task(db: &Store, task: &str, project: &str, repo: &Path) -> Result<()> {
    let project = resolve(db, project)?;
    db.atomic(|| {
        if let Some(owner) = db
            .conn
            .query_row(
                "SELECT project FROM task_projects WHERE task=?",
                [task],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            ensure!(owner == project, "task project ownership is immutable");
            let existing: Option<String> = db.conn.query_row(
                "SELECT repository FROM task_projects WHERE task=?",
                [task],
                |r| r.get(0),
            )?;
            if let Some(existing) = existing {
                ensure!(
                    register_repository(db, &project, repo)? == existing,
                    "task repository ownership is immutable"
                );
            }
            return Ok(());
        }
        let repository = register_repository(db, &project, repo)?;
        db.conn.execute(
            "INSERT INTO task_projects VALUES(?,?,?)",
            params![task, project, repository],
        )?;
        Ok(())
    })
}
pub fn local_runtime(db: &Store) -> Result<String> {
    let cached = crate::management::value(db, "runtime.identity")?;
    let file = db.root.join("network-runtime.toml");
    let identity = if file.exists() {
        match crate::network::NetworkConfig::load(Some(&file)) {
            Ok(config) => config.runtime_id,
            Err(error) => return cached.context(error),
        }
    } else {
        cached.clone().unwrap_or_else(|| "local".to_owned())
    };
    if cached.as_deref() != Some(identity.as_str()) {
        crate::management::set(db, "runtime.identity", &identity)?;
    }
    Ok(identity)
}
/// Permanently dedicate a runtime identity to one project. Existing conflicting
/// explicit grants must be revoked first; implicit default access does not survive.
pub fn bind_runtime(db: &Store, project: &str, runtime: &str) -> Result<()> {
    let project = resolve(db, project)?;
    ensure!(
        !runtime.is_empty() && runtime.len() <= 256,
        "invalid runtime identity"
    );
    let local = local_runtime(db)?;
    let alias = if runtime == "local" {
        local.as_str()
    } else if runtime == local {
        "local"
    } else {
        runtime
    };
    db.atomic(|| {
        let conflict: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_project_bindings WHERE runtime IN (?,?) AND project!=?) OR EXISTS(SELECT 1 FROM project_runtime_grants WHERE runtime IN (?,?) AND project!=?)",
            params![runtime,alias,project,runtime,alias,project], |row| row.get(0),
        )?;
        ensure!(!conflict,"runtime is already bound or granted to another project");
        let identity = if runtime == local { "local" } else { runtime };
        db.conn.execute("INSERT OR IGNORE INTO runtime_project_bindings VALUES(?,?)",params![identity,project])?;
        db.conn.execute("DELETE FROM project_runtime_revocations WHERE project=? AND runtime IN (?,?)",params![project,runtime,alias])?;
        db.conn.execute("INSERT OR IGNORE INTO project_runtime_grants VALUES(?,?,?)",params![project,runtime,now()])?;
        Ok(())
    })
}
pub fn runtime_allowed(db: &Store, project: &str, runtime: &str) -> Result<bool> {
    let project = resolve(db, project)?;
    let local = local_runtime(db)?;
    let alias = if runtime == "local" {
        local.as_str()
    } else if runtime == local {
        "local"
    } else {
        runtime
    };
    let foreign_owner: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime_project_bindings WHERE runtime IN (?,?) AND project!=?)",
        params![runtime,alias,project], |row| row.get(0),
    )?;
    if foreign_owner {
        return Ok(false);
    }
    let revoked: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_runtime_revocations WHERE project=? AND runtime IN (?,?))",
        params![project, runtime, alias],
        |r| r.get(0),
    )?;
    if revoked {
        return Ok(false);
    }
    if project == DEFAULT_PROJECT {
        let managed: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM managed_runtimes WHERE id IN (?,?))",
            params![runtime, alias],
            |row| row.get(0),
        )?;
        if !managed {
            return Ok(true);
        }
    }
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_runtime_grants WHERE project=? AND runtime IN (?,?))",
        params![project, runtime, alias],
        |r| r.get(0),
    )?)
}
fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("{key} is required"))
}
fn validate_slug(slug: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(slug).is_err(),
        "project slug cannot be a UUID"
    );
    ensure!(
        !slug.is_empty()
            && slug.len() <= 64
            && slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
        "project slug must contain 1..64 lowercase letters, digits, hyphens or underscores"
    );
    Ok(())
}
fn options(args: &Value, concurrency: u64, isolation: &str) -> Result<(u64, String)> {
    let concurrency = if args.get("concurrency").is_some() {
        args["concurrency"]
            .as_u64()
            .context("concurrency must be an integer")?
    } else {
        concurrency
    };
    ensure!(
        (1..=64).contains(&concurrency),
        "concurrency must be between 1 and 64"
    );
    let isolation = if let Some(value) = args.get("isolation") {
        value.as_str().context("isolation must be a string")?
    } else {
        isolation
    }
    .to_owned();
    ensure!(
        matches!(isolation.as_str(), "native" | "vm"),
        "isolation must be native or vm"
    );
    Ok((concurrency, isolation))
}
pub fn dispatch(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    let result = match name {
        "project_create" => {
            let slug = args["slug"]
                .as_str()
                .or(args["name"].as_str())
                .context("slug is required")?;
            validate_slug(slug)?;
            let name = args["name"].as_str().unwrap_or(slug);
            ensure!(
                !name.trim().is_empty() && name.len() <= 256,
                "invalid project name"
            );
            let (concurrency, isolation) = options(args, 4, "native")?;
            let id = if let Some(requested) = args["id"].as_str() {
                uuid::Uuid::parse_str(requested)
                    .context("project id must be a UUID")?
                    .to_string()
            } else {
                id()
            };
            let tenant_id = args["tenant_id"].as_str().unwrap_or(&id);
            validate_tenant(tenant_id)?;
            db.atomic(|| {
                db.conn.execute(
                    "INSERT INTO projects VALUES(?,?,?,?,?,?)",
                    params![id, slug, name, concurrency, isolation, now()],
                )?;
                db.conn.execute(
                    "INSERT INTO project_tenants VALUES(?,?,1)",
                    params![id, tenant_id],
                )?;
                Ok(())
            })?;
            json!({"id":id,"slug":slug,"name":name,"tenant_id":tenant_id,"concurrency":concurrency,"isolation":isolation})
        }
        "project_list" => {
            let mut projects = db.rows("SELECT * FROM projects ORDER BY slug", &[])?;
            for project in &mut projects {
                project["tenant_id"] =
                    json!(tenant(db, project["id"].as_str().context("project id")?)?);
            }
            json!(projects)
        }
        "project_inspect" => {
            let project = resolve(db, required(args, "project")?)?;
            let mut value = db
                .rows("SELECT * FROM projects WHERE id=?", &[&project])?
                .remove(0);
            value["tenant_id"] = json!(tenant(db, &project)?);
            value["repositories"] = json!(db.rows(
                "SELECT * FROM project_repositories WHERE project=? ORDER BY path",
                &[&project]
            )?);
            value["runtime_grants"] = json!(db.rows(
                "SELECT runtime FROM project_runtime_grants WHERE project=? ORDER BY runtime",
                &[&project]
            )?);
            value["dedicated_runtimes"] = json!(db.rows(
                "SELECT runtime FROM runtime_project_bindings WHERE project=? ORDER BY runtime",
                &[&project]
            )?);
            value
        }
        "project_update" => {
            let project = resolve(db, required(args, "project")?)?;
            let old = db
                .rows("SELECT * FROM projects WHERE id=?", &[&project])?
                .remove(0);
            let (concurrency, isolation) = options(
                args,
                old["concurrency"].as_u64().context("concurrency")?,
                old["isolation"].as_str().context("isolation")?,
            )?;
            let existing_tenant = tenant(db, &project)?;
            let tenant_id = args["tenant_id"].as_str().unwrap_or(&existing_tenant);
            validate_tenant(tenant_id)?;
            if tenant_id != existing_tenant {
                let occupied: bool = db.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM task_projects WHERE project=?)",
                    [&project],
                    |row| row.get(0),
                )?;
                ensure!(
                    !occupied,
                    "project tenant binding is immutable after its first task"
                );
                let explicit: Option<bool> = db
                    .conn
                    .query_row(
                        "SELECT explicit FROM project_tenants WHERE project=?",
                        [&project],
                        |row| row.get(0),
                    )
                    .optional()?;
                ensure!(
                    explicit != Some(true),
                    "project tenant binding is immutable"
                );
            }
            db.atomic(|| {
                db.conn.execute(
                    "UPDATE projects SET concurrency=?,isolation=? WHERE id=?",
                    params![concurrency, isolation, project],
                )?;
                if tenant_id != existing_tenant {
                    db.conn.execute("INSERT INTO project_tenants VALUES(?,?,1) ON CONFLICT(project) DO UPDATE SET tenant_id=excluded.tenant_id,explicit=1", params![project,tenant_id])?;
                }
                Ok(())
            })?;
            json!({"id":project,"tenant_id":tenant_id,"concurrency":concurrency,"isolation":isolation})
        }
        "project_configure" => {
            let project = resolve(db, required(args, "project")?)?;
            ensure!(
                project != DEFAULT_PROJECT,
                "configure the default project's legacy user config directly"
            );
            let file = Path::new(required(args, "file")?);
            ensure!(
                std::fs::metadata(file)?.len() <= 1024 * 1024,
                "project configuration exceeds 1 MiB"
            );
            let contents = std::fs::read_to_string(file)?;
            let directory = storage_root(db, &project)?;
            std::fs::create_dir_all(&directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
            }
            let stage = tempfile::tempdir_in(&directory)?;
            let staged = stage.path().join("config.toml");
            {
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut output = options.open(&staged)?;
                output.write_all(contents.as_bytes())?;
                output.sync_all()?;
            }
            crate::config::Settings::load_dir(stage.path())?;
            let destination = directory.join("config.toml");
            std::fs::rename(&staged, &destination)?;
            std::fs::File::open(&directory)?.sync_all()?;
            json!({"project":project,"configured":true,"path":destination})
        }
        "project_repo_add" => {
            let project = resolve(db, required(args, "project")?)?;
            let repository = db
                .atomic(|| register_repository(db, &project, Path::new(required(args, "path")?)))?;
            json!({"id":repository,"project":project})
        }
        "project_runtime_grant" | "project_runtime_revoke" => {
            let project = resolve(db, required(args, "project")?)?;
            let runtime = required(args, "runtime")?;
            ensure!(runtime.len() <= 256, "runtime identity too long");
            let local = local_runtime(db)?;
            let alias = if runtime == "local" {
                local.as_str()
            } else if runtime == local {
                "local"
            } else {
                runtime
            };
            db.atomic(|| {
                if name == "project_runtime_grant" {
                    if args["dedicated"] == true {
                        bind_runtime(db,&project,runtime)?;
                    }
                    let conflict: bool = db.conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM runtime_project_bindings WHERE runtime IN (?,?) AND project!=?)",
                        params![runtime,alias,project],|row|row.get(0),
                    )?;
                    ensure!(!conflict,"runtime belongs exclusively to another project");
                    db.conn.execute(
                        "DELETE FROM project_runtime_revocations WHERE project=? AND runtime IN (?,?)",
                        params![project, runtime, alias],
                    )?;
                    db.conn.execute(
                        "INSERT OR IGNORE INTO project_runtime_grants VALUES(?,?,?)",
                        params![project, runtime, now()],
                    )?;
                } else {
                    db.conn.execute(
                        "DELETE FROM project_runtime_grants WHERE project=? AND runtime IN (?,?)",
                        params![project, runtime, alias],
                    )?;
                    db.conn.execute(
                        "INSERT OR IGNORE INTO project_runtime_revocations VALUES(?,?)",
                        params![project, runtime],
                    )?;
                    db.conn.execute(
                        "UPDATE credential_deliveries SET state='revocation_pending' WHERE project=? AND runtime IN (?,?) AND state IN ('delivered','replacement_pending')",
                        params![project,runtime,alias],
                    )?;
                    db.conn.execute(
                        "UPDATE project_remote_reservations SET state='revoked' WHERE project=? AND runtime IN (?,?) AND state='active'",
                        params![project,runtime,alias],
                    )?;
                    db.conn.execute(
                        "UPDATE account_remote_reservations SET state='revoked' WHERE project=? AND runtime IN (?,?) AND state='active'",
                        params![project,runtime,alias],
                    )?;
                }
                Ok(())
            })?;
            json!({"project":project,"runtime":runtime,"granted":name=="project_runtime_grant"})
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}
