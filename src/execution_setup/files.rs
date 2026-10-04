use super::*;
use std::os::unix::fs::PermissionsExt;

pub(super) fn regular(path: &Path) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "execution file must be a regular file"
    );
    Ok(())
}
pub(super) fn existing_directory(path: &Path) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_dir(),
        "execution directory must not be a symlink or file"
    );
    Ok(())
}
pub(super) fn directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        ensure!(
            metadata.file_type().is_dir(),
            "execution directory must not be a symlink or file"
        );
    }
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        regular(path)?;
    }
    let parent = path.parent().context("execution file parent missing")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .map_err(|_| anyhow::anyhow!("cannot persist execution file"))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub(super) fn link(path: &Path, target: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            ensure!(
                std::fs::read_link(path)? == target,
                "execution link points elsewhere; reconcile manually"
            );
            return Ok(());
        }
        ensure!(
            metadata.is_dir() && std::fs::read_dir(path)?.next().is_none(),
            "execution path contains existing data; reconcile manually"
        );
        std::fs::remove_dir(path)?;
    }
    std::os::unix::fs::symlink(target, path)?;
    Ok(())
}
fn document(path: &Path) -> Result<toml_edit::DocumentMut> {
    if !path.try_exists()? {
        return Ok(toml_edit::DocumentMut::new());
    }
    regular(path)?;
    std::fs::read_to_string(path)?
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid existing execution TOML configuration"))
}
pub(super) fn cargo(path: &Path, target: &Path) -> Result<()> {
    let mut config = document(path)?;
    if config
        .get("build")
        .is_some_and(|item| !item.is_table_like())
    {
        anyhow::bail!("existing Cargo build configuration is not a table");
    }
    config["build"]["target-dir"] = toml_edit::value(target.to_str().context("target encoding")?);
    write(path, config.to_string().as_bytes())
}
pub(super) fn codex(path: &Path, paths: &[PathBuf]) -> Result<()> {
    let mut config = document(path)?;
    if config
        .get("sandbox_workspace_write")
        .is_some_and(|item| !item.is_table_like())
    {
        anyhow::bail!("existing Codex sandbox configuration is not a table");
    }
    let existing = config
        .get("sandbox_workspace_write")
        .and_then(|t| t.get("writable_roots"));
    let mut roots = match existing {
        Some(item) => item
            .as_array()
            .context("Codex writable_roots must be an array")?
            .clone(),
        None => toml_edit::Array::new(),
    };
    ensure!(
        roots.iter().all(|v| v.as_str().is_some()),
        "Codex writable_roots must contain strings"
    );
    for path in paths {
        let path = path.to_str().context("writable root encoding")?;
        if !roots.iter().any(|v| v.as_str() == Some(path)) {
            roots.push(path);
        }
    }
    config["sandbox_workspace_write"]["writable_roots"] = toml_edit::value(roots);
    write(path, config.to_string().as_bytes())
}
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn git_config(path: &Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .args(["config", "--file"])
        .arg(path)
        .args(args)
        .output()
        .context("execute git configuration")?;
    ensure!(
        output.status.success(),
        "cannot reconcile Git configuration"
    );
    Ok(())
}
pub(super) fn git(
    root: &Path,
    home: &Path,
    projects: &[Project],
    executable: &Path,
    origin: &str,
    controller: bool,
) -> Result<()> {
    let destination = home.join(".gitconfig");
    let path = if controller {
        root.join("private/git/config")
    } else {
        destination.clone()
    };
    directory(&root.join("private/git"))?;
    // Stage changes so invalid existing configuration cannot be partially rewritten.
    let stage = tempfile::NamedTempFile::new_in(path.parent().context("Git config parent")?)?;
    let mut bytes = Vec::new();
    if path.try_exists()? {
        regular(&path)?;
        bytes.extend(std::fs::read(&path)?);
    }
    if controller && let Ok(metadata) = std::fs::symlink_metadata(&destination) {
        if metadata.file_type().is_symlink() {
            ensure!(
                std::fs::read_link(&destination)? == path,
                "controller Git config points elsewhere"
            );
        } else {
            regular(&destination)?;
            bytes.push(b'\n');
            bytes.extend(std::fs::read(&destination)?);
        }
    }
    write(stage.path(), &bytes)?;
    git_config(stage.path(), &["--list"])?;
    for project in projects {
        let prefix = format!(
            "credential.{}/git/{}",
            origin.trim_end_matches('/'),
            project.slug
        );
        let helper = format!(
            "!{} --data-dir {} git-credential --project-id {}",
            quoted(executable.to_str().context("executable encoding")?),
            quoted(root.to_str().context("root encoding")?),
            quoted(&project.id)
        );
        git_config(
            stage.path(),
            &["--replace-all", &format!("{prefix}.useHttpPath"), "true"],
        )?;
        git_config(
            stage.path(),
            &["--replace-all", &format!("{prefix}.helper"), ""],
        )?;
        git_config(
            stage.path(),
            &["--add", &format!("{prefix}.helper"), &helper],
        )?;
    }
    write(&path, &std::fs::read(stage.path())?)?;
    if controller {
        if std::fs::symlink_metadata(&destination).is_ok_and(|m| m.is_file()) {
            std::fs::remove_file(&destination)?;
        }
        link(&destination, &path)?;
    }
    Ok(())
}
