//! Repository snapshot encoding, limits, and compatibility with older peers.
use base64::Engine;
use horde::federation::{archive, snapshot, unpack};
use serde_json::{Value, json};
use std::{io::Read, path::Path};

const MIB: usize = 1024 * 1024;

fn repository(root: &Path, files: &[(&str, Vec<u8>)]) {
    std::fs::create_dir_all(root).unwrap();
    for (name, body) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    for argv in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "Fixture"],
    ] {
        horde::git::run(root, &argv).unwrap();
    }
}

/// Source-like text: compressible, but not a single repeated byte.
fn source_text(file: usize, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes + 128);
    let mut line = 0usize;
    while out.len() < bytes {
        out.extend_from_slice(
            format!(
                "fn item_{file}_{line}() -> u64 {{ {} }} // {:x}\n",
                line * 31 + file,
                line.wrapping_mul(2654435761)
            )
            .as_bytes(),
        );
        line += 1;
    }
    out.truncate(bytes);
    out
}

fn tar_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append(&header, *body).unwrap();
    }
    archive.into_inner().unwrap()
}

fn gzip_snapshot(bytes: &[u8]) -> Value {
    json!({
        "commit":"fixture",
        "hash":horde::store::hash(bytes),
        "encoding":archive::GZIP_ENCODING,
        "archive":base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

#[test]
fn repository_larger_than_the_uncompressed_limit_round_trips_compressed() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("src/module_{i}.rs"), source_text(i, 5 * MIB)))
        .collect();
    let borrowed: Vec<(&str, Vec<u8>)> = files
        .iter()
        .map(|(name, body)| (name.as_str(), body.clone()))
        .collect();
    repository(&repo, &borrowed);
    let total: usize = files.iter().map(|(_, body)| body.len()).sum();
    assert!(total > archive::MAX_ARCHIVE);

    let compressed = snapshot(&repo).unwrap();
    assert_eq!(compressed["encoding"], archive::GZIP_ENCODING);
    let payload = base64::engine::general_purpose::STANDARD
        .decode(compressed["archive"].as_str().unwrap())
        .unwrap();
    assert!(payload.len() <= archive::MAX_ARCHIVE);
    assert_eq!(compressed["hash"], horde::store::hash(&payload));

    let remote = temp.path().join("remote");
    unpack(&compressed, &remote).unwrap();
    for (name, body) in &files {
        assert_eq!(&std::fs::read(remote.join(name)).unwrap(), body);
    }
    assert!(
        horde::git::run(&remote, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );

    // The same repository cannot be expressed in the legacy form, so an older
    // peer gets a clear refusal instead of a truncated archive.
    let error = archive::for_peer(&compressed, archive::Encoding::Tar).unwrap_err();
    assert!(error.to_string().contains("does not accept compressed"));
    let error = archive::create(&repo, archive::Encoding::Tar).unwrap_err();
    assert!(error.to_string().contains("24 MiB"));
}

#[test]
fn compressed_snapshot_is_converted_for_a_peer_without_support() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    repository(&repo, &[("README.md", b"hello\n".to_vec())]);
    let compressed = snapshot(&repo).unwrap();
    let legacy = archive::for_peer(&compressed, archive::Encoding::Tar).unwrap();
    assert!(legacy.get("encoding").is_none());
    assert_eq!(legacy["commit"], compressed["commit"]);
    let direct = archive::create(&repo, archive::Encoding::Tar).unwrap();
    assert_eq!(legacy, direct);
    let remote = temp.path().join("remote");
    unpack(&legacy, &remote).unwrap();
    assert_eq!(
        std::fs::read_to_string(remote.join("README.md")).unwrap(),
        "hello\n"
    );
}

#[test]
fn legacy_uncompressed_snapshot_is_still_accepted() {
    let bytes = tar_of(&[
        ("README.md", b"legacy\n"),
        ("src/lib.rs", b"pub fn f() {}\n"),
    ]);
    // Exactly the shape older runtimes send: no encoding field, hex archive.
    let snapshot =
        json!({"commit":"old","hash":horde::store::hash(&bytes),"archive":hex::encode(&bytes)});
    let temp = tempfile::tempdir().unwrap();
    let remote = temp.path().join("remote");
    unpack(&snapshot, &remote).unwrap();
    assert_eq!(
        std::fs::read_to_string(remote.join("README.md")).unwrap(),
        "legacy\n"
    );
    assert_eq!(
        std::fs::read_to_string(remote.join("src/lib.rs")).unwrap(),
        "pub fn f() {}\n"
    );
}

#[test]
fn hash_mismatch_is_rejected_for_both_encodings() {
    let bytes = tar_of(&[("README.md", b"hello\n")]);
    let temp = tempfile::tempdir().unwrap();
    let legacy = json!({"hash":horde::store::hash(b"other"),"archive":hex::encode(&bytes)});
    let error = unpack(&legacy, &temp.path().join("legacy")).unwrap_err();
    assert!(error.to_string().contains("integrity"));

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &bytes).unwrap();
    let compressed = encoder.finish().unwrap();
    let mut snapshot = gzip_snapshot(&compressed);
    snapshot["hash"] = json!(horde::store::hash(&bytes));
    let error = unpack(&snapshot, &temp.path().join("compressed")).unwrap_err();
    assert!(error.to_string().contains("integrity"));
    assert!(!temp.path().join("compressed/README.md").exists());
}

#[test]
fn expanded_size_bomb_is_rejected_while_streaming() {
    // Five 60 MiB files of zeros: each is under the per-file limit, together
    // they exceed the 256 MiB expanded limit, and they compress to well under
    // the 24 MiB archive limit.
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for index in 0..5 {
        let size = 60 * MIB as u64;
        let mut header = tar::Header::new_gnu();
        header.set_path(format!("zeros-{index}")).unwrap();
        header.set_size(size);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append(&header, std::io::repeat(0).take(size))
            .unwrap();
    }
    let compressed = builder.into_inner().unwrap().finish().unwrap();
    assert!(compressed.len() < archive::MAX_ARCHIVE);
    let temp = tempfile::tempdir().unwrap();
    let error = unpack(&gzip_snapshot(&compressed), temp.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("expanded snapshot exceeds size limit"),
        "{error}"
    );
    assert!(!temp.path().join("zeros-4").exists());
}

#[test]
fn compressed_snapshots_keep_the_path_and_file_type_checks() {
    let temp = tempfile::tempdir().unwrap();
    let compress = |bytes: &[u8]| {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, bytes).unwrap();
        gzip_snapshot(&encoder.finish().unwrap())
    };
    let metadata = tar_of(&[(".git/hooks/pre-commit", b"#!/bin/sh\n")]);
    let error = unpack(&compress(&metadata), &temp.path().join("git")).unwrap_err();
    assert!(error.to_string().contains("Git metadata"));

    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_path("link").unwrap();
    header.set_link_name("/etc/passwd").unwrap();
    header.set_size(0);
    header.set_cksum();
    archive.append(&header, std::io::empty()).unwrap();
    let error = unpack(
        &compress(&archive.into_inner().unwrap()),
        &temp.path().join("link"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("links and special files"));

    let repo = temp.path().join("secret");
    repository(&repo, &[("config/.env", b"TOKEN=x\n".to_vec())]);
    let error = snapshot(&repo).unwrap_err();
    assert!(error.to_string().contains("tracked secret file"));
}

#[test]
fn runtimes_advertise_compressed_snapshot_support() {
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(&temp.path().join("data")).unwrap();
    let local = horde::capabilities::local(&db).unwrap();
    assert!(
        local["protocol"]["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|feature| feature == archive::GZIP_FEATURE)
    );
}
