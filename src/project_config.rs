//! Resolve workflow configuration without granting repository files host authority.
use super::*;
use crate::{projects, store::Store};
use anyhow::ensure;

impl Settings {
    pub fn load_project_user(db: &Store, project: &str) -> Result<Self> {
        let project = projects::resolve(db, project)?;
        if project == projects::DEFAULT_PROJECT {
            return Self::load_dir(&db.user_config_dir());
        }
        Self::load_dir(&projects::storage_root(db, &project)?)
    }

    pub fn load_project(db: &Store, project: &str, repo: &Path) -> Result<Self> {
        let project = projects::resolve(db, project)?;
        let approved = Self::load_project_user(db, &project)?;
        let mut value = toml::Value::try_from(&approved)?;
        for path in [
            repo.join(".horde.toml"),
            crate::branding::project_config(repo),
        ] {
            if !path.exists() {
                continue;
            }
            let patch: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)
                .with_context(|| format!("invalid {}", path.display()))?;
            validate_repository_settings(repo, &approved, &patch)?;
            merge(&mut value, patch);
        }
        let settings: Self = value.try_into()?;
        settings.validate()?;
        // Sandbox network access widens what an executor can reach, so only
        // user, project, or administrator configuration may grant it. Checking
        // the merged result also stops a repository from moving a role onto
        // a provider the operator opened for a different role.
        for (role, config) in settings.resolved() {
            ensure!(
                !config.network || approved.executor(&role).is_some_and(|c| c.network),
                "repository cannot enable network access for executor {role}; set network in the project or user configuration"
            );
        }
        Ok(settings)
    }
}

fn validate_repository_settings(
    repo: &Path,
    approved: &Settings,
    patch: &toml::Value,
) -> Result<()> {
    ensure!(
        patch.get("providers").is_none(),
        "repository provider connections must be configured by the project administrator"
    );
    ensure!(
        patch.get("decision").is_none(),
        "repository decision configuration must be set by the project administrator"
    );
    ensure!(
        patch.get("automatic_delivery").is_none(),
        "repository automatic delivery configuration must be set by the project administrator"
    );
    if let Some(value) = patch.get("allow_commands").and_then(toml::Value::as_bool) {
        ensure!(
            !value || approved.allow_commands,
            "repository cannot enable commands prohibited by its project"
        );
    }
    if let Some(value) = patch.get("concurrency").and_then(toml::Value::as_integer) {
        ensure!(
            value > 0 && value as usize <= approved.concurrency,
            "repository cannot raise project concurrency"
        );
    }
    if let Some(delivery) = patch.get("delivery") {
        ensure!(
            delivery.get("enabled").and_then(toml::Value::as_bool) != Some(true)
                || approved.delivery.enabled,
            "repository cannot enable project delivery"
        );
        let authorized = toml::Value::try_from(&approved.delivery)?;
        for field in [
            "repository",
            "program",
            "merge",
            "deploy_workflow",
            "health_url",
        ] {
            if let Some(value) = delivery.get(field) {
                ensure!(
                    authorized.get(field) == Some(value),
                    "repository cannot change project delivery {field}"
                );
            }
        }
    }
    if let Some(notify) = patch.get("notify") {
        let authorized = toml::Value::try_from(&approved.notify)?;
        for field in ["webhook", "webhook_env", "command"] {
            if let Some(value) = notify.get(field) {
                ensure!(
                    authorized.get(field) == Some(value),
                    "repository cannot change project notification destination"
                );
            }
        }
    }
    if let Some(bundles) = patch.get("secret_bundles").and_then(toml::Value::as_array) {
        ensure!(
            bundles.iter().all(|v| v
                .as_str()
                .is_some_and(|s| approved.secret_bundles.iter().any(|n| n == s))),
            "repository cannot expand project secret bundles"
        );
    }
    if let Some(executors) = patch.get("executors").and_then(toml::Value::as_table) {
        for executor in executors.values() {
            ensure!(
                executor.get("program").is_none(),
                "repository cannot replace project executor programs"
            );
        }
    }
    if let Some(skills) = patch.get("skills").and_then(toml::Value::as_table) {
        let root = repo.canonicalize()?;
        for value in skills.values() {
            let path = value.as_str().context("skill path must be a string")?;
            ensure!(
                repo.join(path).canonicalize()?.starts_with(&root),
                "repository skill path leaves its checkout"
            );
        }
    }
    Ok(())
}
