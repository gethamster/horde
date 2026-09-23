//! Repository snapshots exchanged with remote runtimes.
//!
//! A snapshot is a JSON object holding one commit's tracked files as a tar
//! archive. Legacy snapshots carry the raw tar as hex. Runtimes that advertise
//! [`GZIP_FEATURE`] also accept `"encoding":"tar+gzip"`, a gzip-compressed tar
//! carried as standard base64. Receivers accept both forms; senders use the
//! compressed form only for a peer that advertises or requests it.
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    io::{Read, Write},
    path::Path,
    process::Stdio,
    rc::Rc,
};

/// Peer capability: the runtime unpacks [`GZIP_ENCODING`] snapshots.
pub const GZIP_FEATURE: &str = "snapshot_gzip";
/// Snapshot `encoding` for a gzip-compressed tar carried as base64.
pub const GZIP_ENCODING: &str = "tar+gzip";
/// Request field listing the snapshot encodings a caller can unpack.
pub const ACCEPT_FIELD: &str = "snapshot_encodings";
/// Largest archive payload, measured in transmitted bytes: compressed bytes
/// for a gzip snapshot, raw tar bytes for a legacy one.
pub const MAX_ARCHIVE: usize = 24 * 1024 * 1024;
/// Largest expanded tar stream read while creating or unpacking a snapshot.
pub const MAX_EXPANDED: u64 = 256 * 1024 * 1024;
/// Largest single file accepted while unpacking.
pub const MAX_ENTRY: u64 = 64 * 1024 * 1024;
/// Output bound for the small Git commands around a transfer.
const MAX_COMMAND_OUTPUT: usize = 24 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Uncompressed tar, hex-encoded. Every runtime version accepts it.
    Tar,
    /// Gzip-compressed tar, base64-encoded.
    TarGzip,
}

impl Encoding {
    /// Pick the encoding a peer's `capabilities` reply advertises.
    pub fn for_features(capabilities: &Value) -> Self {
        if capabilities["features"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == GZIP_FEATURE))
        {
            Self::TarGzip
        } else {
            Self::Tar
        }
    }

    /// Pick the encoding a caller listed in its request arguments.
    pub fn for_request(args: &Value) -> Self {
        if args[ACCEPT_FIELD]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == GZIP_ENCODING))
        {
            Self::TarGzip
        } else {
            Self::Tar
        }
    }

    fn of(snapshot: &Value) -> Result<Self> {
        match snapshot.get("encoding") {
            None | Some(Value::Null) => Ok(Self::Tar),
            Some(value) if value == GZIP_ENCODING => Ok(Self::TarGzip),
            Some(other) => bail!("unsupported snapshot encoding {other}"),
        }
    }
}

/// Encodings this runtime unpacks, for the [`ACCEPT_FIELD`] of a request.
pub fn accepted() -> Value {
    json!([GZIP_ENCODING])
}

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub archive: usize,
    pub expanded: u64,
    pub entry: u64,
}

pub(crate) const LIMITS: Limits = Limits {
    archive: MAX_ARCHIVE,
    expanded: MAX_EXPANDED,
    entry: MAX_ENTRY,
};

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = crate::executor::clean_command("git")
        .args(args)
        .current_dir(repo)
        .output()?;
    ensure!(out.status.success(), "repository transfer command failed");
    ensure!(
        out.stdout.len() <= MAX_COMMAND_OUTPUT,
        "repository transfer command output exceeds 24 MiB limit"
    );
    Ok(out.stdout)
}

/// Snapshot the repository's HEAD commit in the requested encoding.
pub fn create(repo: &Path, encoding: Encoding) -> Result<Value> {
    let commit = String::from_utf8(git(repo, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let paths = String::from_utf8(git(repo, &["ls-tree", "-r", "--name-only", &commit])?)?;
    ensure!(
        !paths.lines().any(|p| Path::new(p)
            .file_name()
            .is_some_and(|n| n == ".env" || n.to_string_lossy().ends_with(".key"))),
        "repository snapshot contains a tracked secret file"
    );
    // Stream `git archive` so neither the tar nor an oversized repository is
    // buffered whole. Compression happens in-process: Git's own tar.gz filter
    // is configurable and could run an arbitrary command.
    let mut child = crate::executor::clean_command("git")
        .args(["archive", "--format=tar", &commit])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().context("repository archive output")?;
    let read = read_archive(stdout, encoding, LIMITS);
    if read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let bytes = read?;
    ensure!(status.success(), "repository transfer command failed");
    let hash = crate::store::hash(&bytes);
    Ok(match encoding {
        Encoding::Tar => json!({"commit":commit,"hash":hash,"archive":hex::encode(bytes)}),
        Encoding::TarGzip => json!({
            "commit":commit,
            "hash":hash,
            "encoding":GZIP_ENCODING,
            "archive":base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
    })
}

fn read_archive(mut tar: impl Read, encoding: Encoding, limits: Limits) -> Result<Vec<u8>> {
    match encoding {
        Encoding::Tar => {
            let mut bytes = Vec::new();
            (&mut tar)
                .take(limits.archive as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= limits.archive,
                "repository archive exceeds the {} uncompressed transfer limit; the receiving runtime does not accept compressed snapshots",
                mib(limits.archive as u64)
            );
            Ok(bytes)
        }
        Encoding::TarGzip => {
            let mut encoder = flate2::write::GzEncoder::new(
                Vec::with_capacity(1024 * 1024),
                flate2::Compression::default(),
            );
            let mut buffer = vec![0; 256 * 1024];
            let mut expanded = 0u64;
            loop {
                let read = tar.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                expanded += read as u64;
                ensure!(
                    expanded <= limits.expanded,
                    "repository archive exceeds the {} expanded transfer limit",
                    mib(limits.expanded)
                );
                encoder.write_all(&buffer[..read])?;
                ensure!(
                    encoder.get_ref().len() <= limits.archive,
                    "compressed repository archive exceeds the {} transfer limit",
                    mib(limits.archive as u64)
                );
            }
            let bytes = encoder.finish()?;
            ensure!(
                bytes.len() <= limits.archive,
                "compressed repository archive exceeds the {} transfer limit",
                mib(limits.archive as u64)
            );
            Ok(bytes)
        }
    }
}

fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
}

/// Decode and verify the transmitted archive bytes.
fn decode(snapshot: &Value, limits: Limits) -> Result<(Vec<u8>, Encoding)> {
    let encoding = Encoding::of(snapshot)?;
    let text = snapshot["archive"].as_str().context("repository archive")?;
    // Bound the text before decoding so an oversized payload is never expanded.
    let max_text = match encoding {
        Encoding::Tar => limits.archive * 2,
        Encoding::TarGzip => limits.archive.div_ceil(3) * 4,
    };
    ensure!(text.len() <= max_text, "snapshot integrity or size failure");
    let bytes = match encoding {
        Encoding::Tar => hex::decode(text)?,
        Encoding::TarGzip => base64::engine::general_purpose::STANDARD.decode(text)?,
    };
    ensure!(
        bytes.len() <= limits.archive && snapshot["hash"] == crate::store::hash(&bytes),
        "snapshot integrity or size failure"
    );
    Ok((bytes, encoding))
}

/// Re-encode a snapshot for a peer. A compressed snapshot is expanded into the
/// legacy form for a peer without [`GZIP_FEATURE`]; any other snapshot is sent
/// unchanged, so a retried send stays byte-identical.
pub fn for_peer(snapshot: &Value, encoding: Encoding) -> Result<Value> {
    if encoding == Encoding::TarGzip || Encoding::of(snapshot)? == Encoding::Tar {
        return Ok(snapshot.clone());
    }
    let (bytes, _) = decode(snapshot, LIMITS)?;
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .take(MAX_ARCHIVE as u64 + 1)
        .read_to_end(&mut tar)?;
    ensure!(
        tar.len() <= MAX_ARCHIVE,
        "repository archive exceeds the 24 MiB uncompressed transfer limit; the receiving runtime does not accept compressed snapshots, so update it"
    );
    Ok(json!({
        "commit":snapshot["commit"],
        "hash":crate::store::hash(&tar),
        "archive":hex::encode(tar),
    }))
}

/// Fails a read once more than `remaining` bytes have been produced.
struct Bounded<R> {
    inner: R,
    remaining: u64,
    exceeded: Rc<Cell<bool>>,
}

impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        if read as u64 > self.remaining {
            self.exceeded.set(true);
            return Err(std::io::Error::other(
                "expanded snapshot exceeds size limit",
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

fn git_metadata_component(component: &std::ffi::OsStr) -> bool {
    // HFS+ ignores these format characters when comparing filenames. APFS and
    // case-sensitive hosts must reject the same archive before it is portable.
    let normalized: String = component.to_string_lossy().chars().filter(|c| {
        !matches!(c, '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}')
    }).collect();
    normalized.eq_ignore_ascii_case(".git")
}

/// Verify a snapshot and extract its files into `path`. The expanded tar is
/// streamed and bounded; nothing is decompressed into memory whole.
pub(crate) fn extract(snapshot: &Value, path: &Path, limits: Limits) -> Result<()> {
    let (bytes, encoding) = decode(snapshot, limits)?;
    std::fs::create_dir_all(path)?;
    let exceeded = Rc::new(Cell::new(false));
    let stream: Box<dyn Read> = match encoding {
        Encoding::Tar => Box::new(bytes.as_slice()),
        Encoding::TarGzip => Box::new(flate2::read::GzDecoder::new(bytes.as_slice())),
    };
    let mut archive = tar::Archive::new(Bounded {
        inner: stream,
        remaining: limits.expanded,
        exceeded: exceeded.clone(),
    });
    let result = (|| -> Result<()> {
        let mut size = 0u64;
        for entry in archive.entries()? {
            let mut e = entry?;
            if e.header().entry_type().is_pax_global_extensions() {
                continue;
            }
            ensure!(
                e.size() <= limits.entry,
                "snapshot entry exceeds size limit"
            );
            size += e.size();
            ensure!(
                size <= limits.expanded,
                "expanded snapshot exceeds size limit"
            );
            ensure!(
                e.header().entry_type().is_file() || e.header().entry_type().is_dir(),
                "snapshot links and special files are unsupported"
            );
            let p = e.path()?.into_owned();
            let s = p.to_str().context("UTF-8 repository paths required")?;
            ensure!(
                !p.components()
                    .any(|p| git_metadata_component(p.as_os_str())),
                "snapshot cannot contain Git metadata"
            );
            crate::store::scope(s)?;
            ensure!(e.unpack_in(path)?, "snapshot path escapes repository");
        }
        Ok(())
    })();
    if result.is_err() && exceeded.get() {
        bail!("expanded snapshot exceeds size limit");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append(&header, body.as_slice()).unwrap();
        }
        archive.into_inner().unwrap()
    }

    fn gzip(bytes: &[u8]) -> Value {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        let bytes = encoder.finish().unwrap();
        json!({"commit":"c","hash":crate::store::hash(&bytes),"encoding":GZIP_ENCODING,"archive":base64::engine::general_purpose::STANDARD.encode(bytes)})
    }

    const SMALL: Limits = Limits {
        archive: 64 * 1024,
        expanded: 256 * 1024,
        entry: 128 * 1024,
    };

    #[test]
    fn expanded_stream_limit_stops_a_compression_bomb() {
        // Each entry is within the per-entry bound, but together they exceed
        // the expanded bound while compressing far below the archive bound.
        let chunk = vec![0u8; 100 * 1024];
        let tar = tar_of(&[("a", chunk.clone()), ("b", chunk.clone()), ("c", chunk)]);
        let snapshot = gzip(&tar);
        assert!(snapshot["archive"].as_str().unwrap().len() < SMALL.archive);
        let dir = tempfile::tempdir().unwrap();
        let error = extract(&snapshot, dir.path(), SMALL).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expanded snapshot exceeds size limit")
        );
        assert!(!dir.path().join("c").exists());
    }

    #[test]
    fn single_entry_limit_applies_to_compressed_snapshots() {
        let tar = tar_of(&[("big", vec![0u8; 200 * 1024])]);
        let dir = tempfile::tempdir().unwrap();
        let error = extract(&gzip(&tar), dir.path(), SMALL).unwrap_err();
        assert!(error.to_string().contains("entry exceeds size limit"));
    }

    #[test]
    fn peers_without_compression_receive_the_legacy_form() {
        let tar = tar_of(&[("file.txt", b"hello\n".to_vec())]);
        let compressed = gzip(&tar);
        let legacy = for_peer(&compressed, Encoding::Tar).unwrap();
        assert!(legacy.get("encoding").is_none());
        assert_eq!(legacy["archive"], hex::encode(&tar));
        assert_eq!(legacy["hash"], crate::store::hash(&tar));
        assert_eq!(legacy["commit"], "c");
        assert_eq!(
            for_peer(&compressed, Encoding::TarGzip).unwrap(),
            compressed
        );
        assert_eq!(for_peer(&legacy, Encoding::TarGzip).unwrap(), legacy);
    }

    #[test]
    fn unknown_encodings_are_rejected() {
        let mut snapshot = gzip(&tar_of(&[("f", b"x".to_vec())]));
        snapshot["encoding"] = json!("tar+zstd");
        let dir = tempfile::tempdir().unwrap();
        let error = extract(&snapshot, dir.path(), LIMITS).unwrap_err();
        assert!(error.to_string().contains("unsupported snapshot encoding"));
    }

    #[test]
    fn requests_and_capabilities_select_the_encoding() {
        assert_eq!(
            Encoding::for_features(&json!({"features":["projects",GZIP_FEATURE]})),
            Encoding::TarGzip
        );
        assert_eq!(
            Encoding::for_features(&json!({"features":["projects"]})),
            Encoding::Tar
        );
        assert_eq!(
            Encoding::for_request(&json!({ACCEPT_FIELD: accepted()})),
            Encoding::TarGzip
        );
        assert_eq!(Encoding::for_request(&json!({})), Encoding::Tar);
    }
}
