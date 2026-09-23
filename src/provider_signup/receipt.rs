use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(super) struct Receipts {
    pub directory: PathBuf,
    _lock: File,
}

impl Drop for Receipts {
    fn drop(&mut self) {
        // A concurrent process spawn can briefly inherit the open description
        // before exec closes it. Release our lock explicitly rather than waiting
        // for every inherited descriptor to close.
        let _ = FileExt::unlock(&self._lock);
    }
}

impl Receipts {
    pub fn open() -> Result<Self> {
        let parent = crate::branding::config_dir();
        std::fs::create_dir_all(&parent)?;
        Self::at(&parent.join("provider-signups"))
    }

    pub(super) fn at(directory: &Path) -> Result<Self> {
        if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(directory) {
            ensure!(
                error.kind() == std::io::ErrorKind::AlreadyExists,
                "signup receipt directory unavailable"
            );
        }
        let metadata = std::fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "signup receipts require a private directory"
        );
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("signup.lock"))?;
        lock.try_lock_exclusive()
            .context("provider signup is busy; retry status shortly")?;
        Ok(Self {
            directory: directory.to_owned(),
            _lock: lock,
        })
    }

    pub fn path(&self, id: &str, extension: &str) -> PathBuf {
        self.directory.join(format!("{id}.{extension}"))
    }

    pub fn read<T: DeserializeOwned>(&self, id: &str) -> Result<T> {
        serde_json::from_slice(&read(&self.path(id, "json"))?)
            .map_err(|_| anyhow::anyhow!("invalid private signup receipt"))
    }

    pub fn write<T: Serialize>(&self, id: &str, value: &T) -> Result<()> {
        self.write_bytes(id, "json", &serde_json::to_vec(value)?)
    }

    pub fn write_bytes(&self, id: &str, extension: &str, bytes: &[u8]) -> Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(self.path(id, extension))?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "signup receipt must be a private regular file"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 128 * 1024, "signup receipt too large");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_is_private_atomic_and_reloads_after_releasing_lock() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("receipts");
        let receipts = Receipts::at(&directory).unwrap();
        receipts
            .write("operation", &serde_json::json!({"state":"submitting"}))
            .unwrap();
        assert_eq!(
            std::fs::metadata(receipts.path("operation", "json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(Receipts::at(&directory).is_err());
        drop(receipts);
        let reopened = Receipts::at(&directory).unwrap();
        let record: serde_json::Value = reopened.read("operation").unwrap();
        assert_eq!(record["state"], "submitting");
    }

    #[test]
    fn receipt_refuses_public_directories_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("public");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Receipts::at(&directory).is_err());
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&directory, &link).unwrap();
        assert!(Receipts::at(&link).is_err());
    }
}
