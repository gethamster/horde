//! Bounded release extraction into a fresh, private staging directory.
use anyhow::{Result, ensure};
use std::{os::unix::fs::PermissionsExt, path::Path};

const BINARY_LIMIT: u64 = 256 * 1024 * 1024;
const SKILL_LIMIT: u64 = 8 * 1024 * 1024;

pub(super) fn extract(bytes: &[u8], stage: &Path) -> Result<()> {
    extract_archive(bytes, stage, false)
}

pub(super) fn extract_skills(bytes: &[u8], stage: &Path) -> Result<()> {
    ensure!(bytes.len() <= 16 * 1024 * 1024, "skills archive too large");
    extract_archive(bytes, stage, true)
}

fn extract_archive(bytes: &[u8], stage: &Path, skills_only: bool) -> Result<()> {
    ensure!(
        bytes.len() <= 272 * 1024 * 1024,
        "release archive too large"
    );
    let mut archive = tar::Archive::new(bytes);
    let mut seen = std::collections::BTreeSet::new();
    let mut skill_bytes = 0;
    let mut found = false;
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        let raw = entry.path_bytes();
        let name = std::str::from_utf8(&raw)?.trim_end_matches('/');
        let kind = entry.header().entry_type();
        ensure!(!skills_only || name != "horde", "binary in skills artifact");
        validate_path(name, kind.is_dir())?;
        ensure!(
            kind.is_file() || kind.is_dir(),
            "unexpected release archive entry type"
        );
        ensure!(
            seen.len() < 4096 && seen.insert(name.to_lowercase()),
            "duplicate or excessive release archive entries"
        );
        let output = stage.join(name);
        if kind.is_dir() {
            ensure!(entry.size() == 0, "release directory has content");
            std::fs::create_dir_all(&output)?;
            continue;
        }
        if name == "horde" {
            ensure!(entry.size() <= BINARY_LIMIT, "release binary too large");
            found = true;
        } else {
            skill_bytes += entry.size();
            ensure!(
                entry.size() <= 1024 * 1024 && skill_bytes <= SKILL_LIMIT,
                "release skills too large"
            );
        }
        let executable = name == "horde" || entry.header().mode()? & 0o111 != 0;
        std::fs::create_dir_all(output.parent().expect("validated relative file"))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        let copied = std::io::copy(&mut entry, &mut file)?;
        ensure!(copied == entry.size(), "truncated release file");
        file.set_permissions(std::fs::Permissions::from_mode(if executable {
            0o755
        } else {
            0o644
        }))?;
        file.sync_all()?;
    }
    ensure!(skills_only || found, "release binary missing");
    ensure!(
        !skills_only || stage.join("skills").is_dir(),
        "release skills missing"
    );
    sync_directories(stage)
}

// Persist directory entries before the updater can publish this staged release.
fn sync_directories(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_directories(&entry.path())?;
        }
    }
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn validate_path(name: &str, directory: bool) -> Result<()> {
    ensure!(
        name.len() <= 1024
            && !name.contains(['\\', ':'])
            && !name.chars().any(char::is_control)
            && name
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && ((!directory && name == "horde")
                || (directory && name == "skills")
                || name.starts_with("skills/")),
        "unexpected release archive path"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(entries: &[(&str, &[u8], tar::EntryType)]) -> Result<Vec<u8>> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(data.len() as u64);
            header.set_mode(0o6755);
            // Set raw bytes so tests can exercise paths Builder normally rejects.
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, *data)?;
        }
        Ok(builder.into_inner()?)
    }

    #[test]
    fn ships_arbitrary_skills_and_supports_legacy_binary_archives() -> Result<()> {
        for skills in [false, true] {
            let temp = tempfile::tempdir()?;
            let mut entries = vec![("horde", b"binary".as_slice(), tar::EntryType::Regular)];
            if skills {
                entries.push((
                    "skills/custom/SKILL.md",
                    b"instructions",
                    tar::EntryType::Regular,
                ));
            }
            extract(&archive(&entries)?, temp.path())?;
            assert_eq!(std::fs::read(temp.path().join("horde"))?, b"binary");
            if skills {
                let skill = temp.path().join("skills/custom/SKILL.md");
                assert_eq!(std::fs::read(&skill)?, b"instructions");
                assert_eq!(
                    std::fs::metadata(skill)?.permissions().mode() & 0o7777,
                    0o755
                );
            }
        }
        Ok(())
    }

    #[test]
    fn enforces_whole_pack_limits_and_binary_presence() -> Result<()> {
        let temp = tempfile::tempdir()?;
        assert!(extract(&archive(&[])?, temp.path()).is_err());
        let data = vec![0; 1024 * 1024];
        let names: Vec<_> = (0..9).map(|n| format!("skills/a/resource{n}")).collect();
        let entries: Vec<_> = names
            .iter()
            .map(|name| (name.as_str(), data.as_slice(), tar::EntryType::Regular))
            .collect();
        assert!(extract(&archive(&entries)?, temp.path()).is_err());
        let temp = tempfile::tempdir()?;
        let names: Vec<_> = (0..4097).map(|n| format!("skills/d{n}")).collect();
        let entries: Vec<_> = names
            .iter()
            .map(|name| (name.as_str(), b"".as_slice(), tar::EntryType::Directory))
            .collect();
        let error = extract(&archive(&entries)?, temp.path()).unwrap_err();
        assert!(error.to_string().contains("excessive"));
        Ok(())
    }

    #[test]
    fn rejects_paths_links_devices_duplicates_and_oversized_files() -> Result<()> {
        for (name, kind) in [
            ("../outside", tar::EntryType::Regular),
            ("skills/a/../../outside", tar::EntryType::Regular),
            ("skills/a/../alias", tar::EntryType::Regular),
            ("skills/a\\alias", tar::EntryType::Regular),
            ("/skills/absolute", tar::EntryType::Regular),
            ("skills/a/link", tar::EntryType::Symlink),
            ("skills/a/hard", tar::EntryType::Link),
            ("skills/a/device", tar::EntryType::Char),
            ("horde", tar::EntryType::Regular),
        ] {
            let temp = tempfile::tempdir()?;
            let bytes = archive(&[
                ("horde", b"binary", tar::EntryType::Regular),
                (name, b"", kind),
            ])?;
            assert!(extract(&bytes, temp.path()).is_err(), "accepted {name:?}");
        }
        let temp = tempfile::tempdir()?;
        let oversized = vec![0; 1024 * 1024 + 1];
        assert!(
            extract(
                &archive(&[
                    ("horde", b"binary", tar::EntryType::Regular),
                    ("skills/a/SKILL.md", &oversized, tar::EntryType::Regular)
                ])?,
                temp.path()
            )
            .is_err()
        );
        Ok(())
    }
}
