//! Component-owned setup operations; filesystem paths are never supplied by clients.
pub(crate) mod project_registration;
use crate::{projects, store::Store};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;
pub fn operational_observations(db: &Store, args: &Value) -> Result<Value> {
    crate::operational_observations::setup(db, args)
}
fn project(db: &Store, id: &str) -> Result<String> {
    uuid::Uuid::parse_str(id).context("project UUID required")?;
    let resolved = projects::resolve(db, id)?;
    ensure!(resolved == id, "project ID must be canonical");
    Ok(resolved)
}
#[derive(Debug)]
struct RuntimeNotQuiet;
impl std::fmt::Display for RuntimeNotQuiet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("runtime must have no active or unsettled work")
    }
}
impl std::error::Error for RuntimeNotQuiet {}
fn quiet(db: &Store) -> Result<()> {
    for (table, states) in [
        ("attempts", "'running','uncertain'"),
        ("account_reservations", "'active','uncertain','revoked'"),
        ("account_allocations", "'active','uncertain','revoked'"),
    ] {
        let count: i64 = db.conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE state IN ({states})"),
            [],
            |r| r.get(0),
        )?;
        if count != 0 {
            return Err(RuntimeNotQuiet.into());
        }
    }
    Ok(())
}
fn no_symlink(path: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        ensure!(!meta.file_type().is_symlink(), "unexpected symbolic link");
    }
    Ok(())
}
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir(target)?;
    std::fs::set_permissions(target, std::fs::metadata(source)?.permissions())?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let to = target.join(entry.file_name());
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, to)?;
        } else if meta.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            ensure!(meta.is_file(), "unsupported workspace file type");
            std::fs::copy(entry.path(), &to)?;
            std::fs::File::open(to)?.sync_all()?;
        }
    }
    std::fs::File::open(target)?.sync_all()?;
    Ok(())
}
pub fn workspaces(db: &Store, args: &Value) -> Result<Value> {
    let list = args["projects"]
        .as_array()
        .context("projects array required")?;
    ensure!(
        !list.is_empty() && list.len() <= 100,
        "invalid project count"
    );
    let root = std::env::var_os("HORDE_EXECUTION_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/workspace".into());
    workspaces_at(db, args, &root)
}
fn workspaces_at(db: &Store, args: &Value, root: &Path) -> Result<Value> {
    let list = args["projects"]
        .as_array()
        .context("projects array required")?;
    ensure!(
        !list.is_empty() && list.len() <= 100,
        "invalid project count"
    );
    ensure!(root.is_absolute(), "workspace root must be absolute");
    no_symlink(root)?;
    let base = root.join(".horde-workspaces");
    no_symlink(&base)?;
    db.atomic(|| {
        quiet(db)?;
        let ids = list
            .iter()
            .map(|value| project(db, value["id"].as_str().context("project id required")?))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len() == ids.len(),
            "duplicate project"
        );
        std::fs::create_dir_all(&base)?;
        for value in list {
            let id = project(db, value["id"].as_str().context("project id required")?)?;
            let parent = projects::storage_root(db, &id)?;
            no_symlink(&parent)?;
            let source = parent.join("workspaces");
            let target = base.join(&id);
            no_symlink(&target)?;
            if std::fs::symlink_metadata(&source).is_ok_and(|m| m.file_type().is_symlink()) {
                ensure!(
                    std::fs::read_link(&source)? == target,
                    "workspace link points elsewhere"
                );
                ensure!(target.is_dir(), "workspace target missing");
                continue;
            }
            ensure!(
                !target.exists(),
                "workspace destination already exists; reconcile manually"
            );
            std::fs::create_dir_all(&parent)?;
            if source.exists() {
                ensure!(source.is_dir(), "workspace source is not a directory");
                // Preserve original until complete; interruption leaves a collision that fails closed.
                copy_tree(&source, &target)?;
                let backup = parent.join("workspaces.pre-api-migration");
                ensure!(!backup.exists(), "migration backup exists");
                std::fs::rename(&source, &backup)?;
                std::os::unix::fs::symlink(&target, &source)?;
                // Backup retained for operator recovery; never delete unreviewed workspace data.
            } else {
                std::fs::create_dir(&target)?;
                std::os::unix::fs::symlink(&target, &source)?;
            }
            std::fs::File::open(&parent)?.sync_all()?;
        }
        Ok(json!({"workspaces_configured":true,"project_count":list.len()}))
    })
}
struct PoolPlan {
    id: String,
    path: std::path::PathBuf,
    contents: String,
    slots: i64,
}
fn ready_accounts(db: &Store, id: &str, provider: &toml::Value) -> Result<Vec<String>> {
    let field = |name| {
        provider
            .get(name)
            .and_then(toml::Value::as_str)
            .context("provider field missing")
    };
    let mut statement = db.conn.prepare(
        "SELECT DISTINCT a.id FROM accounts a
         JOIN account_grants g ON g.account=a.id
         JOIN auth_profiles p ON p.account=a.id
         WHERE g.project=? AND a.provider=? AND a.auth_mode=? AND a.base_url=?
         AND a.state='active' AND a.authenticated=1 AND a.concurrency>0
         AND p.credential_version>0 AND p.kind<>''
         AND (p.expires_at IS NULL OR p.expires_at>?)",
    )?;
    Ok(statement
        .query_map(
            rusqlite::params![
                id,
                field("kind")?,
                field("auth_mode")?,
                field("base_url")?,
                crate::store::now()
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
fn pool_plan(db: &Store, scope: &str, value: &Value) -> Result<PoolPlan> {
    let id = project(db, value.as_str().context("project UUID required")?)?;
    let path = projects::storage_root(db, &id)?.join("config.toml");
    no_symlink(&path)?;
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
    let provider = config
        .get("providers")
        .and_then(|v| v.get("codex"))
        .context("codex provider missing")?;
    let ready = ready_accounts(db, &id, provider)?;
    ensure!(
        ready.len() >= 2,
        "two ready granted matching accounts required"
    );
    let slots = if scope == "foundry" { 2 } else { 1 };
    let previous = config
        .get("concurrency")
        .and_then(toml::Value::as_integer)
        .context("explicit project concurrency required")?;
    ensure!(
        previous == 1 || (scope == "foundry" && previous == 2),
        "unexpected project concurrency"
    );
    config["concurrency"] = toml::Value::Integer(slots);
    for role in ["planner", "worker", "reviewer", "native", "codex"] {
        let executor = config
            .get_mut("executors")
            .and_then(|v| v.get_mut(role))
            .and_then(toml::Value::as_table_mut)
            .context("executor role missing")?;
        if let Some(pin) = executor.get("account") {
            ensure!(
                pin.as_str().is_some_and(|p| ready.iter().any(|r| r == p)),
                "ineligible account pin"
            );
        }
        executor.remove("account");
    }
    let contents = toml::to_string(&config)?;
    let validation = tempfile::tempdir_in(path.parent().context("config directory")?)?;
    std::fs::write(validation.path().join("config.toml"), &contents)?;
    crate::config::Settings::load_dir(validation.path())?;
    Ok(PoolPlan {
        id,
        path,
        contents,
        slots,
    })
}
fn apply_pool_plan(db: &Store, plan: &PoolPlan) -> Result<()> {
    use std::io::Write;
    let mut stage =
        tempfile::NamedTempFile::new_in(plan.path.parent().context("config directory")?)?;
    stage.write_all(plan.contents.as_bytes())?;
    stage.as_file().sync_all()?;
    projects::dispatch(
        db,
        "project_configure",
        &json!({"project":plan.id,"file":stage.path()}),
    )?;
    projects::dispatch(
        db,
        "project_update",
        &json!({"project":plan.id,"concurrency":plan.slots}),
    )?;
    Ok(())
}
pub fn account_pool(db: &Store, args: &Value) -> Result<Value> {
    let scope = args["scope"].as_str().context("scope required")?;
    ensure!(["foundry", "local"].contains(&scope), "invalid scope");
    let list = args["projects"].as_array().context("projects required")?;
    ensure!(
        !list.is_empty() && list.len() <= 100,
        "invalid project count"
    );
    db.atomic(|| {
        quiet(db)?;
        if scope == "foundry" {
            ensure!(
                crate::management::value(db, "concurrency")?.is_none_or(|v| v == "2"),
                "runtime concurrency override requires reconciliation"
            );
        }
        let plans = list
            .iter()
            .map(|value| pool_plan(db, scope, value))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            plans
                .iter()
                .map(|p| &p.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == plans.len(),
            "duplicate project"
        );
        for plan in &plans {
            apply_pool_plan(db, plan)?;
        }
        if scope == "foundry" {
            crate::management::set(db, "concurrency", "2")?;
        }
        Ok(json!({"configured":true,"scope":scope,"project_count":list.len()}))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_project_cannot_escape_root() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path()).unwrap();
        assert!(project(&db, "../outside").is_err());
    }
    #[test]
    fn workspace_copy_preserves_original_and_rejects_collision() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir(&from).unwrap();
        std::fs::write(from.join("file"), "kept").unwrap();
        copy_tree(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("file")).unwrap(), "kept");
        assert!(from.join("file").exists());
        assert!(copy_tree(&from, &to).is_err());
    }
    #[test]
    fn migrates_and_verifies_workspace_without_overwriting_collision() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(&dir.path().join("data")).unwrap();
        let id = projects::dispatch(&db, "project_create", &json!({"slug":"example"}))
            .unwrap()
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let source = projects::storage_root(&db, &id).unwrap().join("workspaces");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("work"), "preserve").unwrap();
        let root = dir.path().join("volume");
        let args = json!({"projects":[{"id":id}]});
        workspaces_at(&db, &args, &root).unwrap();
        workspaces_at(&db, &args, &root).unwrap();
        assert_eq!(
            std::fs::read_to_string(source.join("work")).unwrap(),
            "preserve"
        );
        assert!(
            source
                .parent()
                .unwrap()
                .join("workspaces.pre-api-migration/work")
                .exists()
        );
        std::fs::remove_file(&source).unwrap();
        std::fs::create_dir(&source).unwrap();
        assert!(workspaces_at(&db, &args, &root).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join(".horde-workspaces").join(id).join("work")).unwrap(),
            "preserve"
        );
    }
    #[test]
    fn rejects_pool_without_ready_accounts_and_preserves_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path()).unwrap();
        let id = projects::dispatch(&db, "project_create", &json!({"slug":"example"}))
            .unwrap()
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let root = projects::storage_root(&db, &id).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let content =
            "concurrency = 1\n[providers.codex]\nkind='codex'\nauth_mode='login'\nbase_url=''\n";
        std::fs::write(root.join("config.toml"), content).unwrap();
        assert!(account_pool(&db, &json!({"scope":"local","projects":[id]})).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("config.toml")).unwrap(),
            content
        );
    }
    #[test]
    fn invalid_later_pool_project_leaves_valid_first_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path()).unwrap();
        let id = projects::dispatch(
            &db,
            "project_create",
            &json!({"slug":"example","concurrency":1}),
        )
        .unwrap()
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut accounts = Vec::new();
        for name in ["first", "second"] {
            let account = crate::accounts::dispatch(
                &db,
                "account_create",
                &json!({"project":id,"name":name,"provider":"codex","auth_mode":"login"}),
            )
            .unwrap()
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            db.conn
                .execute("UPDATE accounts SET authenticated=1 WHERE id=?", [&account])
                .unwrap();
            db.conn
                .execute(
                    "UPDATE auth_profiles SET credential_version=1,kind='test' WHERE account=?",
                    [&account],
                )
                .unwrap();
            accounts.push(account);
        }
        let root = projects::storage_root(&db, &id).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let mut config = String::from(
            "concurrency=1\n[providers.codex]\nkind='codex'\nauth_mode='login'\nbase_url=''\n",
        );
        for role in ["planner", "worker", "reviewer", "native", "codex"] {
            config.push_str(&format!(
                "[executors.{role}]\nprovider='codex'\naccount='{}'\n",
                accounts[0]
            ));
        }
        std::fs::write(root.join("config.toml"), &config).unwrap();
        assert!(pool_plan(&db, "foundry", &json!(id)).is_ok());
        assert!(
            account_pool(
                &db,
                &json!({"scope":"foundry","projects":[id,"../invalid"]})
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("config.toml")).unwrap(),
            config
        );
        assert!(
            crate::management::value(&db, "concurrency")
                .unwrap()
                .is_none()
        );
        account_pool(&db, &json!({"scope":"foundry","projects":[id]})).unwrap();
        let updated = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(!updated.contains("account ="));
        assert_eq!(
            crate::management::value(&db, "concurrency")
                .unwrap()
                .as_deref(),
            Some("2")
        );
    }
    #[test]
    fn workspace_refuses_running_and_uncertain_attempts_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(&dir.path().join("data")).unwrap();
        let id = projects::dispatch(&db, "project_create", &json!({"slug":"example"}))
            .unwrap()
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        db.conn.execute_batch("INSERT INTO tasks VALUES('task','test','/tmp','running','{}','{}',0); INSERT INTO steps VALUES('step','task','test','{}','running',NULL); INSERT INTO attempts(id,step,state,started) VALUES('attempt','step','running',0);").unwrap();
        let root = dir.path().join("volume");
        for state in ["running", "uncertain"] {
            db.conn
                .execute("UPDATE attempts SET state=?", [state])
                .unwrap();
            assert!(workspaces_at(&db, &json!({"projects":[{"id":id}]}), &root).is_err());
            assert!(!root.exists());
        }
    }
}
