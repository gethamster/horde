use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Component, Path},
};
#[derive(Debug)]
pub(super) struct Verified {
    pub assets: Value,
    pub native_binary: Option<std::path::PathBuf>,
}
pub(super) fn verify(directory: &Path, tag: &str) -> Result<Verified> {
    let mut assets = Vec::new();
    let mut native_binary = None;
    for target in TARGETS {
        let prefix = format!("adam-{tag}-{target}");
        let name = format!("{prefix}.tar.gz");
        let path = directory.join(&name);
        ensure!(
            std::fs::metadata(&path)?.is_file()
                && std::fs::metadata(&path)?.len() <= 512 * 1024 * 1024,
            "release archive exceeds its bound"
        );
        let mut file = std::fs::File::open(&path)?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        let digest = hex::encode(digest.finalize());
        let checksum = std::fs::read_to_string(directory.join(format!("{name}.sha256")))?;
        let fields = checksum.split_whitespace().collect::<Vec<_>>();
        ensure!(
            fields.len() == 2 && fields[0] == digest && fields[1] == name,
            "release archive checksum identity mismatch"
        );
        let input = std::fs::File::open(&path)?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(input));
        let mut found = false;
        let mut members = std::collections::BTreeSet::new();
        let mut unpacked = 0u64;
        for entry in archive.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.into_owned();
            ensure!(
                name.components()
                    .all(|part| matches!(part, Component::Normal(_))),
                "release archive has an unsafe path"
            );
            ensure!(
                members.insert(name.clone()) && members.len() <= 3,
                "duplicate or excess release archive member"
            );
            unpacked = unpacked
                .checked_add(entry.size())
                .context("release archive size overflow")?;
            ensure!(
                unpacked <= 512 * 1024 * 1024,
                "release archive uncompressed size exceeds bound"
            );
            let entry_type = entry.header().entry_type();
            if entry_type.is_dir() {
                ensure!(
                    name == Path::new(&prefix) && entry.size() == 0,
                    "unexpected release archive directory"
                );
                continue;
            }
            ensure!(
                entry_type.is_file() && entry.size() <= 512 * 1024 * 1024,
                "release archive contains a link, device or oversized entry"
            );
            if name == Path::new(&format!("{prefix}/README.md")) {
                std::io::copy(&mut entry, &mut std::io::sink())?;
                continue;
            }
            ensure!(
                name == Path::new(&format!("{prefix}/adam")) && !found,
                "unexpected or duplicate release archive member"
            );
            found = true;
            ensure!(
                entry.header().mode()? & 0o111 != 0,
                "release binary is not executable"
            );
            let mut header = [0u8; 64];
            entry.read_exact(&mut header)?;
            verify_architecture(&header, target)?;
            if native(target) {
                let binary = directory.join(format!("verified-{target}"));
                let mut output = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&binary)?;
                output.write_all(&header)?;
                std::io::copy(&mut entry, &mut output)?;
                output.sync_all()?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
                }
                native_binary = Some(binary);
            } else {
                std::io::copy(&mut entry, &mut std::io::sink())?;
            }
        }
        ensure!(found, "release archive lacks its Adam executable");
        // Force gzip checksum validation without allowing unbounded trailing data.
        let mut decoder = archive.into_inner();
        let mut trailing = Vec::new();
        std::io::Read::by_ref(&mut decoder)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut trailing)?;
        ensure!(
            trailing.len() <= 1024 * 1024 && trailing.iter().all(|byte| *byte == 0),
            "release archive has excess trailing data"
        );
        assets.push(json!({"name":name,"sha256":digest,"target":target}));
    }
    Ok(Verified {
        assets: json!(assets),
        native_binary,
    })
}
fn native(target: &str) -> bool {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else {
        return false;
    };
    target == format!("{}-{os}", std::env::consts::ARCH)
}
fn verify_architecture(header: &[u8; 64], target: &str) -> Result<()> {
    if target.ends_with("apple-darwin") {
        let cpu = if target.starts_with("aarch64") {
            0x0100000cu32
        } else {
            0x01000007u32
        };
        ensure!(
            header[..4] == [0xcf, 0xfa, 0xed, 0xfe]
                && u32::from_le_bytes(header[4..8].try_into()?) == cpu,
            "release Mach-O architecture mismatch"
        );
    } else {
        let machine = if target.starts_with("aarch64") {
            183u16
        } else {
            62u16
        };
        ensure!(
            header[..5] == [0x7f, b'E', b'L', b'F', 2]
                && header[5] == 1
                && u16::from_le_bytes(header[18..20].try_into()?) == machine,
            "release ELF architecture mismatch"
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(directory: &Path, tag: &str, target: &str, wrong_arch: bool) {
        let prefix = format!("adam-{tag}-{target}");
        let filename = format!("{prefix}.tar.gz");
        let output = std::fs::File::create(directory.join(&filename)).unwrap();
        let encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let mut bytes = vec![0u8; 64];
        if target.contains("darwin") {
            bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            let cpu: u32 = if target.starts_with("aarch64") {
                0x0100000c
            } else {
                0x01000007
            };
            bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
        } else {
            bytes[..5].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2]);
            bytes[5] = 1;
            let machine: u16 = if target.starts_with("aarch64") {
                183
            } else {
                62
            };
            bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        }
        if wrong_arch {
            bytes[4] = 0;
            bytes[18] = 0;
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("{prefix}/adam"), bytes.as_slice())
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        let digest = hex::encode(Sha256::digest(
            std::fs::read(directory.join(&filename)).unwrap(),
        ));
        std::fs::write(
            directory.join(format!("{filename}.sha256")),
            format!("{digest}  {filename}\n"),
        )
        .unwrap();
    }
    #[test]
    fn every_platform_requires_actual_archive_integrity_and_binary_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let tag = "v0.1.15";
        for target in TARGETS {
            fixture(dir.path(), tag, target, false);
        }
        let result = verify(dir.path(), tag).unwrap();
        assert_eq!(result.assets.as_array().unwrap().len(), 4);
        std::fs::write(
            dir.path()
                .join(format!("adam-{tag}-{}.tar.gz.sha256", TARGETS[0])),
            "0".repeat(64),
        )
        .unwrap();
        assert!(verify(dir.path(), tag).is_err());
    }
    #[test]
    fn correctly_checksummed_wrong_architecture_is_not_a_verified_release() {
        let dir = tempfile::tempdir().unwrap();
        let tag = "v0.1.15";
        for target in TARGETS {
            fixture(dir.path(), tag, target, target == TARGETS[1]);
        }
        assert!(verify(dir.path(), tag).is_err());
    }
}
