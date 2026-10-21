//! Immutable Adam CLI release from a qualified merge, never workflow_dispatch.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
mod artifacts;
const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
];
fn tag_action(existing: &Value, tag: &str, sha: &str, has_intent: bool) -> Result<bool> {
    let matches = existing
        .as_array()
        .context("tag reference list")?
        .iter()
        .filter(|row| row["ref"] == format!("refs/tags/{tag}"))
        .collect::<Vec<_>>();
    ensure!(matches.len() <= 1, "ambiguous immutable release tag");
    let Some(existing) = matches.first() else {
        return Ok(true);
    };
    ensure!(
        has_intent,
        "release version is already used; prepare a new unused version"
    );
    ensure!(
        existing["object"]["type"] == "commit" && existing["object"]["sha"] == sha,
        "release tag differs from this task's immutable intent"
    );
    Ok(false)
}
fn select_workflow(runs: &Value, tag: &str, sha: &str) -> Result<Value> {
    let matches = runs
        .as_array()
        .context("release workflow list")?
        .iter()
        .filter(|row| row["event"] == "push" && row["headSha"] == sha && row["headBranch"] == tag)
        .collect::<Vec<_>>();
    ensure!(matches.len() <= 1, "release workflow identity is ambiguous");
    Ok(matches
        .first()
        .map(|row| (*row).clone())
        .unwrap_or(Value::Null))
}
fn stored(i: &crate::executor::Invocation<'_>, name: &str) -> Result<Option<Value>> {
    i.db.rows(
        "SELECT data FROM external_ops WHERE task=? AND name=?",
        &[&i.task, &name],
    )?
    .first()
    .map(|row| {
        serde_json::from_str(row["data"].as_str().context("package release operation")?)
            .map_err(Into::into)
    })
    .transpose()
}
fn save(i: &crate::executor::Invocation<'_>, name: &str, state: &str, data: &Value) -> Result<()> {
    i.db.conn.execute("INSERT INTO external_ops(task,name,state,data) VALUES(?,?,?,?) ON CONFLICT(task,name) DO UPDATE SET state=excluded.state,data=excluded.data",rusqlite::params![i.task,name,state,data.to_string()])?;
    i.db.event(i.task, &format!("{name}.{state}"), data.clone())?;
    Ok(())
}
pub(crate) async fn execute(i: &crate::executor::Invocation<'_>, merged: &Value) -> Result<Value> {
    use crate::delivery::gh;
    let contract = crate::delivery_policy::package_policy(i)?;
    let commit = merged["mergeCommit"]["oid"]
        .as_str()
        .context("qualified merged source commit")?;
    ensure!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid package source commit"
    );
    let source = gh(
        i,
        &[
            "api",
            &format!(
                "repos/{}/contents/Cargo.toml?ref={commit}",
                contract.repository
            ),
        ],
    )
    .await?;
    ensure!(
        source["encoding"] == "base64",
        "release source manifest encoding"
    );
    use base64::Engine;
    let encoded = source["content"]
        .as_str()
        .context("release source manifest")?
        .split_whitespace()
        .collect::<String>();
    ensure!(
        encoded.len() <= 128 * 1024,
        "release source manifest too large"
    );
    let source_bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
    ensure!(
        source_bytes == std::fs::read(i.workspace.join("Cargo.toml"))?,
        "tested workspace manifest differs from merged source"
    );
    let manifest: toml::Value = toml::from_str(std::str::from_utf8(&source_bytes)?)?;
    let version = manifest["workspace"]["package"]["version"]
        .as_str()
        .context("Adam workspace version")?;
    ensure!(
        version.split('.').count() == 3
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())),
        "package release requires a stable semantic version"
    );
    let tag = format!("v{version}");
    let intent = json!({"repository":contract.repository,"workflow":contract.workflow,"commit":commit,"tag":tag,"policy_hash":crate::store::hash(&serde_json::to_vec(&i.settings.automatic_delivery)?)});
    let prior = stored(i, "package-release.intent")?;
    if let Some(prior) = &prior {
        ensure!(*prior == intent, "package release intent is immutable");
    }
    let endpoint = format!("repos/{}/git/matching-refs/tags/{tag}", contract.repository);
    let refs = gh(i, &["api", &endpoint]).await?;
    let create = tag_action(&refs, &tag, commit, prior.is_some())?;
    if create {
        crate::upstream_maintenance::check_online(i.db, i.task).await?;
        crate::delivery_policy::package_policy(i)?;
        let base = gh(
            i,
            &[
                "api",
                &format!(
                    "repos/{}/branches/{}",
                    contract.repository, i.settings.delivery.base
                ),
            ],
        )
        .await?;
        ensure!(
            base["commit"]["sha"] == commit,
            "main moved before immutable tag publication; requalify the current candidate"
        );
        let check = crate::executor::run_command_env(
            &[
                "cargo".into(),
                "run".into(),
                "--locked".into(),
                "-p".into(),
                "xtask".into(),
                "--".into(),
                "check-release-version".into(),
                "--tag".into(),
                tag.clone(),
            ],
            i.workspace,
            i.settings.timeout_seconds,
            Some((i.db, i.attempt)),
            &Default::default(),
        )
        .await?;
        ensure!(
            check["success"] == true,
            "Adam release version check failed"
        );
        if prior.is_none() {
            save(i, "package-release.intent", "intent", &intent)?;
        }
        crate::upstream_maintenance::check_online(i.db, i.task).await?;
        crate::delivery_policy::package_policy(i)?;
        gh(
            i,
            &[
                "api",
                "--method",
                "POST",
                &format!("repos/{}/git/refs", contract.repository),
                "-f",
                &format!("ref=refs/tags/{tag}"),
                "-f",
                &format!("sha={commit}"),
            ],
        )
        .await?;
    }
    let refs = gh(i, &["api", &endpoint]).await?;
    ensure!(
        !tag_action(&refs, &tag, commit, true)?,
        "immutable tag was not published"
    );
    save(i, "package-release.tag", "succeeded", &intent)?;
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(i.settings.timeout_seconds);
    let run = loop {
        let runs = gh(
            i,
            &[
                "run",
                "list",
                "--repo",
                &contract.repository,
                "--workflow",
                &contract.workflow,
                "--commit",
                commit,
                "--json",
                "databaseId,headSha,headBranch,event,status,conclusion",
                "--limit",
                "100",
            ],
        )
        .await?;
        let run = select_workflow(&runs, &tag, commit)?;
        if !run.is_null() {
            break run;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "tag-push release workflow is not observable; reconcile this same intent"
        );
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    };
    if let Some(prior) = stored(i, "package-release.workflow")? {
        ensure!(
            prior["databaseId"] == run["databaseId"],
            "release workflow identity changed"
        );
    }
    save(i, "package-release.workflow", "observing", &run)?;
    let id = run["databaseId"]
        .as_u64()
        .context("release workflow ID")?
        .to_string();
    gh(
        i,
        &[
            "run",
            "watch",
            &id,
            "--repo",
            &contract.repository,
            "--exit-status",
        ],
    )
    .await?;
    let checked = gh(
        i,
        &[
            "run",
            "view",
            &id,
            "--repo",
            &contract.repository,
            "--json",
            "databaseId,headSha,headBranch,event,status,conclusion",
        ],
    )
    .await?;
    ensure!(
        checked["databaseId"] == run["databaseId"]
            && checked["headSha"] == commit
            && checked["headBranch"] == tag
            && checked["event"] == "push"
            && checked["status"] == "completed"
            && checked["conclusion"] == "success",
        "release workflow did not qualify the exact tag and source"
    );
    let release = gh(
        i,
        &[
            "release",
            "view",
            &tag,
            "--repo",
            &contract.repository,
            "--json",
            "tagName,isDraft,isPrerelease,url,assets",
        ],
    )
    .await?;
    ensure!(
        release["tagName"] == tag
            && release["isDraft"] == false
            && release["isPrerelease"] == false,
        "release is not publicly published for the exact stable tag"
    );
    let asset_names = release["assets"]
        .as_array()
        .context("release assets")?
        .iter()
        .filter_map(|asset| asset["name"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        TARGETS.iter().all(|target| {
            let name = format!("adam-{tag}-{target}.tar.gz");
            asset_names.contains(name.as_str())
                && asset_names.contains(format!("{name}.sha256").as_str())
        }),
        "release lacks one of four platform archives/checksums"
    );
    let directory = tempfile::tempdir_in(&i.db.root)?;
    gh(
        i,
        &[
            "release",
            "download",
            &tag,
            "--repo",
            &contract.repository,
            "--dir",
            directory
                .path()
                .to_str()
                .context("package verification directory")?,
            "--pattern",
            &format!("adam-{tag}-*.tar.gz"),
            "--pattern",
            &format!("adam-{tag}-*.tar.gz.sha256"),
        ],
    )
    .await?;
    let verified = artifacts::verify(directory.path(), &tag)?;
    let binary = verified
        .native_binary
        .context("package verification needs a supported native host")?;
    let version_output = crate::executor::run_command_env(
        &[
            binary.to_str().context("native binary path")?.into(),
            "--version".into(),
        ],
        directory.path(),
        30,
        Some((i.db, i.attempt)),
        &Default::default(),
    )
    .await?;
    ensure!(
        version_output["success"] == true
            && version_output["stdout"]
                .as_str()
                .is_some_and(|text| text.trim() == format!("adam {version}")),
        "published native Adam binary version does not match release"
    );
    let final_refs = gh(i, &["api", &endpoint]).await?;
    ensure!(
        !tag_action(&final_refs, &tag, commit, true)?,
        "published tag is missing during final verification"
    );
    crate::upstream_maintenance::check_online(i.db, i.task).await?;
    crate::delivery_policy::package_policy(i)?;
    let result = json!({"repository":contract.repository,"tag":tag,"commit":commit,"workflow_run_id":run["databaseId"],"release_url":release["url"],"assets":verified.assets,"release_assets":release["assets"],"native_version":version,"verified":true});
    save(i, "package-release.verified", "succeeded", &result)?;
    Ok(
        json!({"result":release["url"],"accepted":true,"delivery":"package_released","release":result}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_existing_tag_is_reconciled_only_against_this_tasks_immutable_intent() {
        let tag = "v0.1.15";
        let sha = "a".repeat(40);
        assert!(tag_action(&json!([]), tag, &sha, false).unwrap());
        let existing = json!([{"ref":"refs/tags/v0.1.15","object":{"type":"commit","sha":sha}}]);
        assert!(tag_action(&existing, tag, &sha, false).is_err());
        assert!(!tag_action(&existing, tag, &sha, true).unwrap());
        assert!(tag_action(&existing, tag, &"b".repeat(40), true).is_err());
    }
    #[test]
    fn workflow_is_bound_to_exact_tag_commit_and_push_event() {
        let sha = "a".repeat(40);
        let runs = json!([{"databaseId":1,"headSha":sha,"headBranch":"main","event":"push"},{"databaseId":2,"headSha":sha,"headBranch":"v0.1.15","event":"workflow_dispatch"},{"databaseId":3,"headSha":sha,"headBranch":"v0.1.15","event":"push"}]);
        assert_eq!(
            select_workflow(&runs, "v0.1.15", &sha).unwrap()["databaseId"],
            3
        );
        assert!(select_workflow(&json!([runs[2], runs[2]]), "v0.1.15", &sha).is_err());
    }
}
