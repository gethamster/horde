//! Delivery owns development profiles. Setup callers provide identities, never file paths.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

mod files;
#[cfg(test)]
mod tests;

const LEGACY_GIT_PROXY_ORIGIN: &str = "http://deliver-bridge:8090";
fn default_git_proxy_origin() -> String {
    LEGACY_GIT_PROXY_ORIGIN.to_owned()
}
fn legacy_git_proxy_origin(origin: &str) -> bool {
    origin == LEGACY_GIT_PROXY_ORIGIN
}
fn git_proxy_origin(origin: &str) -> Result<String> {
    if origin == LEGACY_GIT_PROXY_ORIGIN {
        return Ok("deliver-bridge:8090".to_owned());
    }
    let authority = origin
        .strip_prefix("http://")
        .context("execution Git proxy must use HTTP")?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let address: std::net::SocketAddr = authority.parse().map_err(|_| {
        anyhow::anyhow!("execution Git proxy requires a literal loopback address and explicit port")
    })?;
    ensure!(
        address.ip().is_loopback() && address.port() > 0 && address.to_string() == authority,
        "execution Git proxy must be a canonical loopback HTTP origin with explicit nonzero port"
    );
    Ok(authority.to_owned())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    id: String,
    slug: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    home: PathBuf,
    workspace: PathBuf,
    rustup: PathBuf,
    target: PathBuf,
    executable: PathBuf,
    #[serde(
        default = "default_git_proxy_origin",
        skip_serializing_if = "legacy_git_proxy_origin"
    )]
    git_proxy_origin: String,
}
impl Layout {
    fn configured() -> Result<Self> {
        fn path(name: &str, fallback: &str) -> PathBuf {
            std::env::var_os(name).map_or_else(|| PathBuf::from(fallback), PathBuf::from)
        }
        let layout = Self {
            home: path("HORDE_EXECUTION_HOME", "/home/horde"),
            workspace: path("HORDE_EXECUTION_WORKSPACE_ROOT", "/workspace"),
            rustup: path("HORDE_EXECUTION_RUSTUP_ROOT", "/usr/local/rustup"),
            target: path("HORDE_EXECUTION_TARGET_ROOT", "/cache/target"),
            executable: std::env::current_exe()?,
            git_proxy_origin: match std::env::var("HORDE_EXECUTION_GIT_PROXY_ORIGIN") {
                Ok(origin) => origin,
                Err(std::env::VarError::NotPresent) => default_git_proxy_origin(),
                Err(_) => anyhow::bail!("execution Git proxy origin must be valid UTF-8"),
            },
        };
        layout.validate()?;
        Ok(layout)
    }
    fn validate(&self) -> Result<()> {
        git_proxy_origin(&self.git_proxy_origin)?;
        for path in [
            &self.home,
            &self.workspace,
            &self.rustup,
            &self.target,
            &self.executable,
        ] {
            ensure!(
                path.is_absolute()
                    && !path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "execution paths must be absolute without parent traversal"
            );
            ensure!(
                path.to_str()
                    .is_some_and(|p| !p.contains(['\n', '\r', '\0'])),
                "invalid execution path"
            );
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    schema_version: u32,
    projects: Vec<Project>,
    #[serde(default)]
    layout: Option<Layout>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    scope: String,
    projects: Vec<ProjectRequest>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRequest {
    id: String,
    slug: String,
    git_proxy_token: String,
}

fn load(root: &Path) -> Result<Option<Profile>> {
    let path = root.join("private/execution.json");
    if !path.try_exists()? {
        return Ok(None);
    }
    files::existing_directory(&root.join("private"))?;
    files::regular(&path)?;
    ensure!(
        std::fs::metadata(&path)?.len() <= 1024 * 1024,
        "execution profile exceeds size limit"
    );
    let profile: Profile = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| anyhow::anyhow!("invalid persisted execution profile"))?;
    ensure!(
        profile.schema_version == 1,
        "unsupported execution profile version"
    );
    for project in &profile.projects {
        identity(project)?;
    }
    if let Some(layout) = &profile.layout {
        layout.validate()?;
    }
    Ok(Some(profile))
}
fn identity(project: &Project) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(&project.id).is_ok_and(|id| id.to_string() == project.id),
        "execution project requires canonical UUID"
    );
    ensure!(
        !project.slug.is_empty()
            && project.slug.len() <= 128
            && project.slug.as_bytes()[0].is_ascii_lowercase()
            && project
                .slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid execution project slug"
    );
    Ok(())
}
fn lock(root: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    use std::os::unix::fs::OpenOptionsExt;
    let directory = root.join("private");
    files::directory(&directory)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("execution.lock"))?;
    file.lock_exclusive()?;
    Ok(file)
}

pub fn configure(db: &Store, args: &Value) -> Result<Value> {
    // Validate the complete request before creating files or resolving layout.
    let request = validate(db, args)?;
    configure_request(db, request, Layout::configured()?)
}
fn validate(db: &Store, args: &Value) -> Result<Request> {
    let request: Request = serde_json::from_value(args.clone())
        .map_err(|_| anyhow::anyhow!("invalid execution configuration"))?;
    ensure!(
        ["foundry", "local"].contains(&request.scope.as_str()),
        "invalid installation scope"
    );
    ensure!(
        !request.projects.is_empty() && request.projects.len() <= 1000,
        "execution requires 1-1000 projects"
    );
    let mut ids = BTreeSet::new();
    for project in &request.projects {
        identity(&Project {
            id: project.id.clone(),
            slug: project.slug.clone(),
        })?;
        ensure!(ids.insert(&project.id), "duplicate execution project");
        ensure!(
            project.git_proxy_token.len() >= 32
                && project.git_proxy_token.len() <= 4096
                && project
                    .git_proxy_token
                    .bytes()
                    .all(|b| b.is_ascii_graphic()),
            "invalid scoped Git credential"
        );
        let actual: String = db
            .conn
            .query_row("SELECT slug FROM projects WHERE id=?", [&project.id], |r| {
                r.get(0)
            })
            .context("unknown execution project")?;
        ensure!(
            actual == project.slug,
            "execution project slug does not match identity"
        );
    }
    Ok(request)
}
fn configure_request(db: &Store, request: Request, layout: Layout) -> Result<Value> {
    layout.validate()?;
    let _lock = lock(&db.root)?;
    let mut projects = load(&db.root)?.map_or_else(Vec::new, |p| p.projects);
    for project in request.projects {
        let credential_dir = db.root.join("private/git").join(&project.id);
        files::directory(&db.root.join("private/git"))?;
        files::directory(&credential_dir)?;
        files::write(
            &credential_dir.join("token"),
            project.git_proxy_token.as_bytes(),
        )?;
        if let Some(existing) = projects.iter_mut().find(|p| p.id == project.id) {
            *existing = Project {
                id: project.id,
                slug: project.slug,
            };
        } else {
            projects.push(Project {
                id: project.id,
                slug: project.slug,
            });
        }
    }
    let profile = Profile {
        schema_version: 1,
        projects,
        layout: Some(layout.clone()),
    };
    files::write(
        &db.root.join("private/execution.json"),
        &serde_json::to_vec(&profile)?,
    )?;
    let count = restore_profile(db, &profile, &layout)?;
    Ok(
        json!({"profiles_configured": count,"controller_configured":true,"provider_accounts_imported":false}),
    )
}

pub fn restore(db: &Store) -> Result<()> {
    let bootstrap = std::env::var_os("HORDE_EXECUTION_HOME")
        .map(|_| Layout::configured())
        .transpose()?;
    restore_with_bootstrap(db, bootstrap)
}

fn restore_with_bootstrap(db: &Store, bootstrap: Option<Layout>) -> Result<()> {
    if load(&db.root)?.is_none() && bootstrap.is_none() {
        return Ok(());
    }
    let _lock = lock(&db.root)?;
    let Some(profile) = load(&db.root)? else {
        let layout = bootstrap.context("execution profile disappeared")?;
        layout.validate()?;
        // This link must exist in the daemon container before an admin sidecar
        // installs its first profile on the shared data volume.
        return development_home(&db.root, &layout.home, &[], &layout, true);
    };
    let layout = profile.layout.clone().map_or_else(Layout::configured, Ok)?;
    layout.validate()?;
    restore_profile(db, &profile, &layout)?;
    Ok(())
}
fn restore_profile(db: &Store, profile: &Profile, layout: &Layout) -> Result<usize> {
    development_home(&db.root, &layout.home, &profile.projects, layout, true)?;
    let mut count = 0;
    for project in &profile.projects {
        for row in db.rows("SELECT a.id FROM accounts a JOIN account_grants g ON g.account=a.id WHERE g.project=? AND a.state='active' AND a.authenticated=1", &[&project.id])? {
            let account = row["id"].as_str().context("invalid account identity")?;
            account_profile(db, project, account, layout)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Called immediately before starting every managed account, including new grants.
pub fn ensure_account(db: &Store, project: &str, account: &str) -> Result<()> {
    if !db.root.join("private/execution.json").try_exists()? {
        return Ok(());
    }
    let _lock = lock(&db.root)?;
    let Some(profile) = load(&db.root)? else {
        return Ok(());
    };
    let Some(project) = profile.projects.iter().find(|p| p.id == project) else {
        return Ok(());
    };
    let layout = profile.layout.clone().map_or_else(Layout::configured, Ok)?;
    layout.validate()?;
    account_profile(db, project, account, &layout)
}
fn account_profile(db: &Store, project: &Project, account: &str, layout: &Layout) -> Result<()> {
    crate::accounts::authorized(db, &project.id, account)?;
    let root = crate::accounts::profile_directory(&db.root, &project.id, account)?;
    for path in [
        db.root.join("projects"),
        db.root.join("projects").join(&project.id),
        db.root.join("projects").join(&project.id).join("accounts"),
        root.clone(),
    ] {
        files::directory(&path)?;
    }
    development_home(
        &db.root,
        &root.join("home"),
        std::slice::from_ref(project),
        layout,
        false,
    )?;
    files::directory(&root.join("codex"))?;
    files::codex(
        &root.join("codex/config.toml"),
        &[
            layout.workspace.join(&project.slug).join(".git"),
            layout.target.clone(),
            layout.home.join(".cargo/registry"),
            layout.home.join(".cargo/git"),
        ],
    )
}
fn development_home(
    root: &Path,
    home: &Path,
    projects: &[Project],
    layout: &Layout,
    controller: bool,
) -> Result<()> {
    files::directory(home)?;
    files::link(&home.join(".rustup"), &layout.rustup)?;
    files::directory(&home.join(".cargo"))?;
    files::cargo(&home.join(".cargo/config.toml"), &layout.target)?;
    for name in ["registry", "git"] {
        let shared = layout.home.join(".cargo").join(name);
        files::directory(&shared)?;
        if home != layout.home {
            files::link(&home.join(".cargo").join(name), &shared)?;
        }
    }
    files::git(
        root,
        home,
        projects,
        &layout.executable,
        &layout.git_proxy_origin,
        controller,
    )
}

/// Implements Git's credential-helper protocol without an interpreter or secret argv.
pub fn credential_helper(
    root: &Path,
    project: &str,
    operation: &str,
    input: &str,
) -> Result<String> {
    if operation != "get" {
        return Ok(String::new());
    }
    ensure!(input.len() <= 65536, "credential request too large");
    let Some(profile) = load(root)? else {
        return Ok(String::new());
    };
    let Some(project) = profile.projects.iter().find(|p| p.id == project) else {
        return Ok(String::new());
    };
    let mut fields = std::collections::BTreeMap::new();
    for line in input.lines().take_while(|line| !line.is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Ok(String::new());
        };
        if fields.insert(key, value).is_some() {
            return Ok(String::new());
        }
    }
    let host = git_proxy_origin(
        profile
            .layout
            .as_ref()
            .map_or(LEGACY_GIT_PROXY_ORIGIN, |layout| {
                layout.git_proxy_origin.as_str()
            }),
    )?;
    if fields.get("protocol") != Some(&"http")
        || fields.get("host") != Some(&host.as_str())
        || fields.get("path").map(|s| s.trim_end_matches('/'))
            != Some(format!("git/{}", project.slug).as_str())
    {
        return Ok(String::new());
    }
    let path = root.join("private/git").join(&project.id).join("token");
    files::existing_directory(&root.join("private/git"))?;
    files::existing_directory(&root.join("private/git").join(&project.id))?;
    files::regular(&path)?;
    ensure!(
        std::fs::metadata(&path)?.len() <= 4096,
        "stored Git credential exceeds size limit"
    );
    let token = std::fs::read_to_string(path)?;
    ensure!(
        token.len() >= 32 && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_graphic()),
        "invalid stored Git credential"
    );
    Ok(format!("username=horde\npassword={token}\n\n"))
}
