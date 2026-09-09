//! Default skill delivery uses an ordinary signed release artifact, independently of the binary tar.
use super::{Artifact, Manifest, archive, download, verify};
use crate::{management, skill_catalog, skills::Packet, store::Store};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::path::Path;

pub async fn install_release_skills(root: &Path, version: &str) -> Result<Value> {
    let packet = release_packet(version).await?;
    let db = Store::open(root)?;
    let mut result = management::install_catalog(&db, &packet)?;
    result["release"] = version.into();
    Ok(result)
}

pub async fn ensure_default_skills(root: &Path) -> Result<()> {
    if !needs_bootstrap(root)? {
        return Ok(());
    }
    let packet = release_packet(env!("CARGO_PKG_VERSION")).await?;
    if needs_bootstrap(root)? {
        management::bootstrap_catalog(&Store::open(root)?, &packet)?;
    }
    Ok(())
}

fn exists(path: &Path) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn needs_bootstrap(root: &Path) -> Result<bool> {
    if exists(&root.join("skill-packs/CURRENT"))? {
        return Ok(false);
    }
    let executable = std::env::current_exe()?;
    if exists(
        &executable
            .parent()
            .context("executable parent")?
            .join("skills"),
    )? {
        return Ok(false);
    }
    Ok(skill_catalog::load_for(root).is_err())
}

fn artifact(manifest: &Manifest) -> Result<Option<&Artifact>> {
    let matches: Vec<_> = manifest
        .artifacts
        .iter()
        .filter(|a| a.target == "skills")
        .collect();
    ensure!(matches.len() <= 1, "duplicate release skills artifact");
    Ok(matches.first().copied())
}

pub(super) async fn stage(
    client: &reqwest::Client,
    manifest: &Manifest,
    root: &Path,
) -> Result<()> {
    if let Some(artifact) = artifact(manifest)? {
        let bytes = download(client, &artifact.url, 16 * 1024 * 1024).await?;
        extract(artifact, &bytes, root)?;
        skill_catalog::load_from(&root.join("skills"))?;
    }
    Ok(())
}

fn extract(artifact: &Artifact, bytes: &[u8], root: &Path) -> Result<()> {
    ensure!(
        crate::store::hash(bytes) == artifact.sha256,
        "release skills checksum mismatch"
    );
    archive::extract_skills(bytes, root)
}

async fn release_packet(version: &str) -> Result<Packet> {
    super::validate_version(version)?;
    let key = option_env!("HORDE_RELEASE_PUBLIC_KEY").context(
        "this build has no release verification key; install skills from a local directory",
    )?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let base = format!("https://horde.sh/releases/v{version}");
    let bytes = download(&client, &format!("{base}/manifest.json"), 1024 * 1024).await?;
    let signature = download(&client, &format!("{base}/manifest.sig"), 64).await?;
    let manifest = verify(&bytes, &signature, &hex::decode(key)?)?;
    ensure!(
        manifest.version == version,
        "release skills version mismatch"
    );
    artifact(&manifest)?.context("release does not include a skill pack")?;
    let temporary = tempfile::tempdir()?;
    stage(&client, &manifest, temporary.path()).await?;
    skill_catalog::load_from(&temporary.path().join("skills"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_artifact_hash_binds_extracted_skill_files() -> Result<()> {
        let body = b"---\nname: independently-added\ndescription: Example policy.\n---\nUse local tools.\n";
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(
            &mut header,
            "skills/independently-added/SKILL.md",
            body.as_slice(),
        )?;
        let bytes = archive.into_inner()?;
        let artifact = Artifact {
            target: "skills".into(),
            sha256: crate::store::hash(&bytes),
            url: "https://horde.sh/releases/v0.6.0/skills.tar".into(),
        };
        let root = tempfile::tempdir()?;
        extract(&artifact, &bytes, root.path())?;
        let packet = skill_catalog::load_from(&root.path().join("skills"))?;
        assert!(packet.contains_key("independently-added"));
        let root = tempfile::tempdir()?;
        assert!(
            extract(&artifact, b"tampered", root.path())
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
        assert!(!root.path().join("skills").exists());
        Ok(())
    }

    #[tokio::test]
    async fn invalid_existing_pack_is_never_bootstrapped_over() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("skill-packs"))?;
        std::fs::write(root.path().join("skill-packs/CURRENT"), "invalid")?;
        assert!(!needs_bootstrap(root.path())?);
        ensure_default_skills(root.path()).await?;
        assert_eq!(
            std::fs::read_to_string(root.path().join("skill-packs/CURRENT"))?,
            "invalid"
        );
        assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn legacy_manifest_skips_skill_download_but_duplicates_fail_closed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let client = reqwest::Client::new();
        let legacy = Manifest {
            version: "0.6.0".into(),
            protocol: 1,
            schema_min: 2,
            schema_max: 4,
            artifacts: vec![Artifact {
                target: "legacy-platform".into(),
                sha256: "unused".into(),
                url: "no-network-request".into(),
            }],
            image: None,
        };
        stage(&client, &legacy, root.path()).await?;
        assert!(std::fs::read_dir(root.path())?.next().is_none());
        let duplicate = Manifest {
            artifacts: (0..2)
                .map(|_| Artifact {
                    target: "skills".into(),
                    sha256: "unused".into(),
                    url: "no-network-request".into(),
                })
                .collect(),
            ..legacy
        };
        assert!(
            stage(&client, &duplicate, root.path())
                .await
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        assert!(std::fs::read_dir(root.path())?.next().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn existing_development_defaults_need_no_download_or_install() -> Result<()> {
        let root = tempfile::tempdir()?;
        let existing = skill_catalog::load_for(root.path())?;
        assert!(!existing.is_empty());
        assert!(!needs_bootstrap(root.path())?);
        ensure_default_skills(root.path()).await?;
        assert!(!root.path().join("skill-packs").exists());
        assert_eq!(
            skill_catalog::summary(&skill_catalog::load_for(root.path())?)?,
            skill_catalog::summary(&existing)?
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_release_version_fails_before_download_or_pack_mutation() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("skill-packs"))?;
        let pointer = root.path().join("skill-packs/CURRENT");
        std::fs::write(&pointer, "preserve-user-state")?;
        let error = install_release_skills(root.path(), "../unexpected")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid release version"));
        assert_eq!(std::fs::read_to_string(pointer)?, "preserve-user-state");
        Ok(())
    }
}
