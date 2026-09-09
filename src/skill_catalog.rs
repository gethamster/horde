//! Mutable file catalogs become immutable task-owned bundles at submission.
use crate::skills::{self, Packet};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub fn report(packet: &Packet) -> Result<Value> {
    skills::validate(packet)?;
    Ok(
        json!({"hash":crate::store::hash(&serde_json::to_vec(packet)?),"skills":packet.keys().collect::<Vec<_>>()}),
    )
}

pub fn summary(packet: &Packet) -> Result<Value> {
    report(packet)
}

/// Read direct skill directories; ancillary files at the catalog root are ignored.
pub fn load_from(path: &Path) -> Result<Packet> {
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "skill catalog must be a regular directory"
    );
    let canonical = path.canonicalize()?;
    let path = canonical.as_path();
    let mut configured = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "skill catalog cannot contain symlinks"
        );
        if metadata.is_dir() && entry.path().join("SKILL.md").try_exists()? {
            configured.insert(
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("skill name must be UTF-8"))?,
                entry.path(),
            );
            ensure!(configured.len() <= 64, "at most 64 skills can be pinned");
        }
    }
    skills::capture_catalog(path, &configured)
}

fn default_locations() -> Result<Vec<PathBuf>> {
    let executable = std::env::current_exe()?;
    let adjacent = executable
        .parent()
        .context("executable directory")?
        .join("skills");
    let mut locations = vec![adjacent];
    // Development binaries use the checked-out files on every submission.
    // Compare canonical paths: `current_exe` resolves symlinks, so a checkout
    // reached through one (macOS `/tmp` -> `/private/tmp`) would otherwise never
    // match its own `target/` and every development binary would report no pack.
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = checkout.join("target");
    let target = target.canonicalize().unwrap_or(target);
    if cfg!(debug_assertions) && executable.canonicalize()?.starts_with(&target) {
        locations.push(checkout.join("skills"));
    }
    Ok(locations)
}

fn default_catalog() -> Result<(Packet, PathBuf)> {
    for path in default_locations()? {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let packet = load_from(&path)
                    .with_context(|| format!("reading skill pack {}", path.display()))?;
                return Ok((packet, path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!(
        "no skill pack installed; source builds need the repository's skills directory copied next to the executable"
    )
}

pub(crate) fn load_defaults() -> Result<Packet> {
    default_catalog().map(|(packet, _)| packet)
}

fn resolve(root: &Path) -> Result<(Packet, PathBuf)> {
    let pack_root = root.join("skill-packs");
    let current = pack_root.join("CURRENT");
    if fs::symlink_metadata(&current)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return default_catalog();
    }
    for directory in [&pack_root, &pack_root.join("versions")] {
        ensure!(
            fs::symlink_metadata(directory)
                .with_context(|| format!("reading {}", directory.display()))?
                .is_dir(),
            "skill pack path must be a regular directory"
        );
    }
    let metadata = fs::symlink_metadata(&current)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 64,
        "invalid current skill pack pointer"
    );
    let digest = fs::read_to_string(current)?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid current skill pack hash"
    );
    let path = pack_root.join("versions").join(&digest);
    let packet =
        load_from(&path).with_context(|| format!("reading skill pack {}", path.display()))?;
    ensure!(
        report(&packet)?["hash"] == digest,
        "installed skill pack hash mismatch"
    );
    Ok((packet, path))
}

fn locations(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = vec![root.join("skill-packs/CURRENT")];
    paths.extend(default_locations()?);
    Ok(paths)
}

fn checked(root: &Path) -> Result<(Packet, PathBuf)> {
    resolve(root).map_err(|error| {
        let paths = locations(root)
            .unwrap_or_default()
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!("default skill pack check failed: {error:#}; lookup locations (in priority order): {paths}")
    })
}

pub fn load_for(root: &Path) -> Result<Packet> {
    checked(root).map(|(packet, _)| packet)
}

/// Use the submission resolver without fetching, installing, or changing a pack.
pub fn check(root: &Path) -> Result<Value> {
    let (packet, path) = checked(root)?;
    let mut result = report(&packet)?;
    result["path"] = json!(path);
    result["lookup_locations"] = json!(locations(root)?);
    Ok(result)
}

fn directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "skill pack path must be a regular directory"
    );
    Ok(())
}
fn write_new(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.set_permissions(fs::Permissions::from_mode(if executable {
        0o500
    } else {
        0o400
    }))?;
    file.sync_all()?;
    Ok(())
}
fn write_packet(path: &Path, packet: &Packet) -> Result<()> {
    for (name, bundle) in packet {
        let skill_root = path.join(name);
        directory(&skill_root)?;
        for (relative, file) in &bundle.files {
            let target = skill_root.join(relative);
            let mut parent = skill_root.clone();
            for component in Path::new(relative)
                .parent()
                .context("skill file parent")?
                .components()
            {
                parent.push(component);
                directory(&parent)?;
            }
            write_new(&target, &hex::decode(&file.hex)?, file.executable)?;
        }
    }
    sync_directories(path)
}
fn sync_directories(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_directories(&entry.path())?;
        }
    }
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Atomically activate a validated immutable pack for future submissions only.
pub fn install(root: &Path, packet: &Packet) -> Result<Value> {
    let result = report(packet)?;
    let digest = result["hash"].as_str().context("skill pack hash")?;
    let pack_root = root.join("skill-packs");
    directory(&pack_root)?;
    let versions = pack_root.join("versions");
    directory(&versions)?;
    let destination = versions.join(digest);
    if !destination.try_exists()? {
        let staging = versions.join(format!(".staging-{}", crate::store::id()));
        directory(&staging)?;
        let staged = (|| -> Result<()> {
            write_packet(&staging, packet)?;
            ensure!(
                report(&load_from(&staging)?)? == result,
                "staged skill pack hash mismatch"
            );
            match fs::rename(&staging, &destination) {
                Ok(()) => (),
                Err(error) if destination.is_dir() => {
                    fs::remove_dir_all(&staging).context(error.to_string())?;
                }
                Err(error) => return Err(error.into()),
            }
            fs::File::open(&versions)?.sync_all()?;
            Ok(())
        })();
        if staged.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        staged?;
    }
    ensure!(
        report(&load_from(&destination)?)? == result,
        "installed skill pack hash mismatch"
    );
    let temporary: PathBuf = pack_root.join(format!(".current-{}", crate::store::id()));
    write_new(&temporary, digest.as_bytes(), false)?;
    fs::rename(temporary, pack_root.join("CURRENT"))?;
    fs::File::open(&pack_root)?.sync_all()?;
    // Persist the first skill-packs directory entry before recording success.
    fs::File::open(root)?.sync_all()?;
    Ok(result)
}
