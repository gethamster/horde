//! Stable GitHub publication identity, distinct from merely finding a tag.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
pub(super) fn identity(
    value: &Value,
    repository: &str,
    tag: &str,
    prior: Option<&Value>,
) -> Result<Value> {
    let id = value["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("published release ID unavailable")?;
    ensure!(
        value["tag_name"] == tag
            && value["draft"] == false
            && value["prerelease"] == false
            && value["immutable"] == true
            && value["published_at"]
                .as_str()
                .is_some_and(|at| !at.is_empty())
            && value["url"] == format!("https://api.github.com/repos/{repository}/releases/{id}")
            && value["html_url"] == format!("https://github.com/{repository}/releases/tag/{tag}"),
        "release is not an immutable published stable release for the exact repository and tag"
    );
    let mut assets = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for asset in value["assets"]
        .as_array()
        .context("release assets unavailable")?
    {
        let asset_id = asset["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .context("release asset ID unavailable")?;
        let name = asset["name"]
            .as_str()
            .context("release asset name unavailable")?;
        let digest = asset["digest"]
            .as_str()
            .and_then(|s| s.strip_prefix("sha256:"))
            .context("release asset SHA-256 unavailable")?;
        ensure!(
            !name.is_empty()
                && !name.contains('/')
                && !name.contains('\\')
                && name != "."
                && name != ".."
                && asset["state"] == "uploaded"
                && asset["size"].as_u64().is_some_and(|size| size > 0)
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                && asset["url"]
                    == format!(
                        "https://api.github.com/repos/{repository}/releases/assets/{asset_id}"
                    )
                && asset["browser_download_url"]
                    == format!("https://github.com/{repository}/releases/download/{tag}/{name}")
                && ids.insert(asset_id),
            "release asset identity is missing, ambiguous or outside the publication"
        );
        let snapshot = json!({"id":asset_id,"name":name,"size":asset["size"],"digest":asset["digest"],"url":asset["url"],"browser_download_url":asset["browser_download_url"]});
        ensure!(
            assets.insert(name.to_owned(), snapshot).is_none(),
            "duplicate release asset name"
        );
    }
    ensure!(
        TARGETS.iter().all(|target| {
            let name = format!("adam-{tag}-{target}.tar.gz");
            assets.contains_key(&name) && assets.contains_key(&format!("{name}.sha256"))
        }),
        "release lacks one of four platform archives/checksums"
    );
    let snapshot = json!({"id":id,"tag_name":tag,"url":value["url"],"html_url":value["html_url"],"immutable":true,"published_at":value["published_at"],"assets":assets.into_values().collect::<Vec<_>>()});
    if let Some(prior) = prior {
        ensure!(
            *prior == snapshot,
            "published release or asset identity changed; reconcile this same intent"
        );
    }
    Ok(snapshot)
}
pub(super) fn verify_files(directory: &Path, publication: &Value, tag: &str) -> Result<()> {
    for target in TARGETS {
        for suffix in ["", ".sha256"] {
            let name = format!("adam-{tag}-{target}.tar.gz{suffix}");
            let asset = publication["assets"]
                .as_array()
                .context("pinned release assets")?
                .iter()
                .find(|asset| asset["name"] == name)
                .context("pinned release asset missing")?;
            let path = directory.join(&name);
            let metadata = std::fs::symlink_metadata(&path)?;
            let limit = if suffix.is_empty() {
                512 * 1024 * 1024
            } else {
                1024
            };
            ensure!(
                metadata.is_file()
                    && metadata.len() <= limit
                    && asset["size"].as_u64() == Some(metadata.len()),
                "downloaded file size differs from immutable release asset"
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
            ensure!(
                asset["digest"] == format!("sha256:{}", hex::encode(digest.finalize())),
                "downloaded bytes differ from immutable release asset digest"
            );
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn release() -> Value {
        let repo = "gethamster/adam";
        let tag = "v0.1.15";
        let mut assets = Vec::new();
        for (index, target) in TARGETS.iter().enumerate() {
            for (suffix, offset) in [("", 0), (".sha256", 1)] {
                let id = index * 2 + offset + 1;
                let name = format!("adam-{tag}-{target}.tar.gz{suffix}");
                assets.push(json!({"id":id,"name":name,"size":3,"digest":format!("sha256:{}","a".repeat(64)),"state":"uploaded","url":format!("https://api.github.com/repos/{repo}/releases/assets/{id}"),"browser_download_url":format!("https://github.com/{repo}/releases/download/{tag}/{name}")}));
            }
        }
        json!({"id":71,"url":format!("https://api.github.com/repos/{repo}/releases/71"),"html_url":format!("https://github.com/{repo}/releases/tag/{tag}"),"tag_name":tag,"draft":false,"prerelease":false,"immutable":true,"published_at":"2026-10-20T00:00:00Z","assets":assets})
    }
    #[test]
    fn mutable_or_missing_immutability_is_never_a_verified_package_publication() {
        let original = release();
        assert!(identity(&original, "gethamster/adam", "v0.1.15", None).is_ok());
        for flag in [json!(false), Value::Null] {
            let mut changed = original.clone();
            changed["immutable"] = flag;
            assert!(
                identity(&changed, "gethamster/adam", "v0.1.15", None).is_err(),
                "mutable or unproven publication was accepted"
            );
        }
    }
    #[test]
    fn downloaded_checksums_and_archives_must_match_pinned_github_asset_digests() {
        let dir = tempfile::tempdir().unwrap();
        let mut release = release();
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(b"abc")));
        for asset in release["assets"].as_array_mut().unwrap() {
            asset["digest"] = json!(digest);
            std::fs::write(dir.path().join(asset["name"].as_str().unwrap()), b"abc").unwrap();
        }
        let publication = identity(&release, "gethamster/adam", "v0.1.15", None).unwrap();
        verify_files(dir.path(), &publication, "v0.1.15").unwrap();
        let name = publication["assets"][0]["name"].as_str().unwrap();
        std::fs::write(dir.path().join(name), b"def").unwrap();
        assert!(
            verify_files(dir.path(), &publication, "v0.1.15").is_err(),
            "same-size replacement escaped the immutable asset digest"
        );
    }
    #[test]
    fn retries_pin_exact_release_and_asset_identity_while_ignoring_download_counts() {
        let original = release();
        let prior = identity(&original, "gethamster/adam", "v0.1.15", None).unwrap();
        let mut counters = original.clone();
        counters["assets"][0]["download_count"] = json!(999);
        assert!(identity(&counters, "gethamster/adam", "v0.1.15", Some(&prior)).is_ok());
        for edit in [
            (|v: &mut Value| {
                v["id"] = json!(72);
                v["url"] = json!("https://api.github.com/repos/gethamster/adam/releases/72");
            }) as fn(&mut Value),
            |v: &mut Value| v["assets"][0]["digest"] = json!(format!("sha256:{}", "b".repeat(64))),
            |v: &mut Value| v["assets"][0]["id"] = json!(991),
            |v: &mut Value| v["assets"][0]["digest"] = Value::Null,
            |v: &mut Value| v["assets"].as_array_mut().unwrap().clear(),
        ] {
            let mut changed = original.clone();
            edit(&mut changed);
            assert!(
                identity(&changed, "gethamster/adam", "v0.1.15", Some(&prior)).is_err(),
                "changed or missing release identity was reconciled"
            );
        }
    }
}
