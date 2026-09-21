//! Compact guest names retain full controller-owned identities.
use super::identifier;
use crate::fleet::Profile;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn resource_name(p: &Profile, id: &str) -> Result<String> {
    ensure!(
        identifier(&p.project) && identifier(id),
        "invalid Lima ownership identity"
    );
    let digest = crate::store::hash(format!("{}:{id}", p.project).as_bytes());
    Ok(format!("h{}", &digest[..11]))
}

fn canonical_home(home: &Path) -> Result<PathBuf> {
    if home.exists() {
        return Ok(home.canonicalize()?);
    }
    let parent = home.parent().context("Lima home parent required")?;
    Ok(canonical_home(parent)?.join(home.file_name().context("Lima home required")?))
}

/// Lima reserves a temporary suffix on its SSH control socket during creation.
pub fn validate_socket_path(home: &Path, resource: &str, os: &str) -> Result<()> {
    ensure!(
        home.is_absolute() && identifier(resource),
        "invalid Lima socket path"
    );
    let socket = canonical_home(home)?
        .join(resource)
        .join("ssh.sock.1234567890123456");
    let limit = if os == "macos" { 104 } else { 108 };
    ensure!(
        socket.as_os_str().len() < limit,
        "Lima SSH socket path exceeds host limit ({limit} bytes); use a shorter approved Lima home"
    );
    Ok(())
}

pub fn owned_resource(root: &Path, p: &Profile, id: &str) -> Result<String> {
    let compact = resource_name(p, id)?;
    let manifest = root
        .join("projects")
        .join(&p.project)
        .join("lima")
        .join(id)
        .join("ownership.json");
    ensure!(
        manifest.is_file(),
        "Lima resource has no durable ownership intent"
    );
    let prior: Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
    ensure!(
        prior["project"] == p.project && prior["runtime"] == id,
        "Lima resource ownership mismatch"
    );
    let legacy = format!("horde-{id}");
    let resource = prior["resource"].as_str().unwrap_or(&legacy);
    ensure!(
        resource == compact || resource == legacy,
        "Lima resource ownership mismatch"
    );
    Ok(resource.into())
}

pub(super) fn claim_resource(root: &Path, p: &Profile, id: &str, resource: &str) -> Result<()> {
    use std::io::Write;
    let home = canonical_home(&p.lima_home)?;
    let scope = crate::store::hash(home.as_os_str().as_encoded_bytes());
    let directory = root.join("lima-resource-claims").join(scope);
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{resource}.json"));
    let identity = json!({"project":p.project,"runtime":id,"home":home,"resource":resource});
    let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
    staged.write_all(identity.to_string().as_bytes())?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                std::fs::symlink_metadata(&path)?.is_file(),
                "Lima resource claim must be a regular file"
            );
            let existing: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(
                existing == identity,
                "Lima resource name collision; retain existing resource for reconciliation"
            );
        }
        Err(error) => return Err(error.error.into()),
    }
    sync_directories(root, &directory)
}

/// Commit new directory entries along with their private ownership files.
pub(super) fn sync_directories(root: &Path, directory: &Path) -> Result<()> {
    ensure!(
        directory.starts_with(root),
        "Lima ownership directory outside runtime root"
    );
    for ancestor in directory.ancestors() {
        std::fs::File::open(ancestor)?.sync_all()?;
        if ancestor == root {
            return Ok(());
        }
    }
    anyhow::bail!("Lima ownership root missing")
}
