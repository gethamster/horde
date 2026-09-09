//! Repository onboarding. Agent choice does not change worker-provider settings.
use crate::{config::Settings, repo_init::Agent};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
};

pub fn run(path: &Path, agent: Agent, data_dir: Option<&Path>) -> Result<Value> {
    ensure!(
        crate::branding::var_os("HORDE_WORKER_TOKEN").is_none(),
        "horde init is for the personal agent; Horde workers must use their assigned coordination bridge"
    );
    let repo = repository(path)?;
    let settings = Settings::load(&repo)?;
    let binary = std::env::current_exe()?;
    let root = absolute(data_dir.unwrap_or(&crate::branding::data_dir()))?;
    let (providers, mut blockers) = provider_checks(&repo, &settings, agent)?;
    let runtime_skills = match crate::skill_catalog::check(&root) {
        Ok(report) => json!({"status":"ready", "pack":report}),
        Err(error) => {
            blockers.push(format!("Repair the runtime skill pack in {} with `horde skills install PATH --data-dir DATA_DIR`, then rerun horde init: {error}", root.display()));
            json!({"status":"unavailable"})
        }
    };
    let pin_root = data_dir.is_some()
        || std::env::var_os("HORDE_DATA_DIR").is_some_and(|path| !path.is_empty());
    let installed = crate::repo_init::install(&repo, agent, pin_root.then_some(root.as_path()))?;
    let simulation = crate::init_smoke::verify(&binary)?;
    let ready = blockers.is_empty();
    let daemon = if ready {
        start_daemon(&binary, &root)?
    } else {
        json!({"started":false,"reason":"Resolve the reported prerequisites, then rerun horde init."})
    };
    Ok(json!({
        "installed":true,"repository":repo,"files":installed,"delegate":"always",
        "ready":ready,"providers":providers,"provider_authentication":"not_probed",
        "simulation":simulation,"runtime_skills":runtime_skills,"daemon":daemon,"next_steps":blockers,
        "agent_reload":"Restart or reload your agent and approve the repository's MCP configuration if prompted."
    }))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn repository(path: &Path) -> Result<PathBuf> {
    let root = git_output(path, &["rev-parse", "--show-toplevel"])
        .context("run horde init inside a Git repository, or supply --repo PATH")?;
    let repo = PathBuf::from(root).canonicalize()?;
    git_output(&repo, &["rev-parse", "--verify", "HEAD"])
        .context("the repository needs an initial commit before horde init")?;
    git_output(&repo, &["var", "GIT_AUTHOR_IDENT"])
        .context("configure Git user.name and user.email before horde init")?;
    Ok(repo)
}

fn git_output(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .context("Git is required for horde init")?;
    ensure!(
        output.status.success(),
        "Git repository check failed: {}",
        args.join(" ")
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn executable(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let paths = if program.contains('/') {
        vec![PathBuf::from(program)]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(program))
            .collect()
    };
    paths.iter().any(|p| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

fn provider_checks(
    repo: &Path,
    settings: &Settings,
    agent: Agent,
) -> Result<(Vec<Value>, Vec<String>)> {
    let template = crate::template::compile(
        &settings.default_template,
        &crate::template::load_templates(&crate::branding::templates(repo))?,
        std::collections::BTreeMap::from([("task".into(), "Check Horde setup".into())]),
    )?;
    let roles: BTreeSet<_> = template
        .steps
        .iter()
        .filter(|s| s.kind == "agent")
        .map(|s| s.role.clone())
        .collect();
    let client = match agent {
        Agent::Codex => "codex",
        Agent::Claude => "claude",
    };
    let mut blockers = Vec::new();
    if !executable(client) {
        blockers.push(format!(
            "Install {client} and sign in, then rerun horde init."
        ));
    }
    if !executable("horde") {
        blockers
            .push("Put horde on PATH for this shell and your agent, then rerun horde init.".into());
    }
    if !settings.allow_commands && template.steps.iter().any(|step| step.kind == "command") {
        blockers.push("The configured workflow has command steps; enable allow_commands or choose a compatible workflow.".into());
    }
    let mut providers = Vec::new();
    for role in roles {
        let config = settings
            .executor(&role)
            .with_context(|| format!("unconfigured executor role {role}"))?;
        let provider = settings.executors[&role].provider();
        blockers.extend(executor_blockers(&role, &config, settings.allow_commands));
        if (config.auth_mode == "api" || config.kind == "tuara")
            && crate::config::credential(&config.api_key_env).is_err()
        {
            blockers.push(format!("Role {role} needs credentials for provider {provider}. Run `horde config provider add {provider}`, or select an existing login provider with `horde config provider add {client} --use-for planner,worker,reviewer`."));
        }
        providers.push(json!({"role":role,"provider":provider,"kind":config.kind,"auth_mode":config.auth_mode,"authentication":"not_probed"}));
    }
    blockers.sort();
    blockers.dedup();
    Ok((providers, blockers))
}

fn executor_blockers(
    role: &str,
    config: &crate::config::ExecutorConfig,
    commands: bool,
) -> Vec<String> {
    let mut blockers = Vec::new();
    match config.kind.as_str() {
        "codex" | "claude" => {
            let program = config.program.as_deref().unwrap_or(&config.kind);
            if !executable(program) {
                blockers.push(format!("Role {role} needs executable {program}; install it or change the configured provider."));
            }
            if !commands {
                blockers.push(format!(
                    "Role {role} uses a CLI worker and requires allow_commands=true."
                ));
            }
        }
        "tuara" => {
            if !executable("rg") {
                blockers.push("Install ripgrep (rg) for native workers.".into());
            }
        }
        "simulated" => {}
        kind => blockers.push(format!(
            "Role {role} uses unsupported executor kind {kind}."
        )),
    }
    if matches!(config.kind.as_str(), "codex" | "tuara") && config.max_api_cost_usd.is_some() {
        blockers.push(format!("Role {role} cannot enforce max_api_cost_usd with {}; choose a supported executor or remove that cap.", config.kind));
    }
    blockers
}

fn start_daemon(binary: &Path, root: &Path) -> Result<Value> {
    let output = Command::new(binary)
        .arg("--data-dir")
        .arg(root)
        .arg("start")
        .output()?;
    ensure!(
        output.status.success(),
        "repository setup is installed, but daemon startup failed; inspect {}/daemon.log",
        root.display()
    );
    serde_json::from_slice(&output.stdout).context("invalid daemon startup response")
}
