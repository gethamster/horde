//! External writes are reconciled by stable task branch and persisted GitHub identifiers.
use crate::executor::{Invocation, run_command};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
async fn gh(i: &Invocation<'_>, args: &[&str]) -> Result<Value> {
    let argv = std::iter::once(
        i.settings
            .delivery
            .program
            .clone()
            .unwrap_or_else(|| "gh".to_owned()),
    )
    .chain(args.iter().map(|x| x.to_string()))
    .collect::<Vec<_>>();
    let out = run_command(
        &argv,
        i.workspace,
        i.settings.timeout_seconds,
        Some((i.db, i.attempt)),
    )
    .await?;
    if out["success"] != true {
        bail!("GitHub command failed: {}", out["stderr"]);
    }
    let text = out["stdout"].as_str().unwrap_or("");
    Ok(serde_json::from_str(text).unwrap_or(json!(text.trim())))
}
fn save(i: &Invocation<'_>, name: &str, state: &str, data: &Value) -> Result<()> {
    i.db.conn.execute("INSERT INTO external_ops VALUES(?,?,?,?) ON CONFLICT(task,name) DO UPDATE SET state=excluded.state,data=excluded.data",rusqlite::params![i.task,name,state,data.to_string()])?;
    i.db.event(i.task, &format!("delivery.{name}.{state}"), data.clone())?;
    Ok(())
}
pub async fn execute(i: &Invocation<'_>) -> Result<Value> {
    let d = &i.settings.delivery;
    if !d.enabled || d.repository.is_empty() || d.base.is_empty() {
        bail!("delivery requires enabled=true, repository, and base in TOML settings");
    }
    let remote = gh(i, &["repo", "view", "--json", "nameWithOwner"]).await?;
    if !remote["nameWithOwner"]
        .as_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(&d.repository))
    {
        bail!("configured delivery repository does not match checkout origin");
    }
    let branch = format!("horde/{}", i.task);
    let head = crate::git::run(i.workspace, &["rev-parse", "HEAD"])?;
    let list = gh(
        i,
        &[
            "pr",
            "list",
            "--repo",
            &d.repository,
            "--head",
            &branch,
            "--state",
            "all",
            "--json",
            "number,url,state,headRefOid,mergedAt",
        ],
    )
    .await?;
    let existing = list
        .as_array()
        .context("malformed GitHub PR list")?
        .first()
        .cloned();
    let pr = if let Some(pr) = existing {
        if pr["state"] == "CLOSED" {
            bail!("task PR was closed without merging; reconcile explicitly");
        }
        pr
    } else {
        save(i, "pr", "intent", &json!({"branch":branch,"head":head}))?;
        let out = run_command(
            &[
                "git".into(),
                "push".into(),
                "origin".into(),
                format!("HEAD:refs/heads/{branch}"),
            ],
            i.workspace,
            i.settings.timeout_seconds,
            Some((i.db, i.attempt)),
        )
        .await?;
        if out["success"] != true {
            bail!("push failed: {}", out["stderr"]);
        }
        let body = i.db.root.join(format!("pr-{}.md", i.task));
        std::fs::write(
            &body,
            format!(
                "{}\n\nImplemented and verified by task `{}`.\n\nIntegrated head: `{head}`\n",
                i.spec.instructions, i.task
            ),
        )?;
        gh(
            i,
            &[
                "pr",
                "create",
                "--repo",
                &d.repository,
                "--base",
                &d.base,
                "--head",
                &branch,
                "--title",
                &i.spec.instructions.chars().take(120).collect::<String>(),
                "--body-file",
                body.to_str().context("body path")?,
            ],
        )
        .await?;
        gh(
            i,
            &[
                "pr",
                "view",
                &branch,
                "--repo",
                &d.repository,
                "--json",
                "number,url,state,headRefOid,mergedAt",
            ],
        )
        .await?
    };
    save(i, "pr", "succeeded", &pr)?;
    let number = pr["number"].as_u64().context("PR number")?.to_string();
    if pr["headRefOid"] != head {
        bail!("PR head differs from verified integrated head; reconciliation required");
    }
    if pr["state"] != "MERGED" {
        save(i, "checks", "observing", &json!({"pr":number,"head":head}))?;
        gh(
            i,
            &[
                "pr",
                "checks",
                &number,
                "--repo",
                &d.repository,
                "--watch",
                "--fail-fast",
            ],
        )
        .await?;
        save(i, "checks", "succeeded", &json!({"head":head}))?;
        if !d.merge {
            return Ok(json!({"result":pr["url"],"accepted":true,"delivery":"pr_ready"}));
        }
        save(i, "merge", "intent", &json!({"pr":number,"head":head}))?;
        gh(
            i,
            &[
                "pr",
                "merge",
                &number,
                "--repo",
                &d.repository,
                "--squash",
                "--match-head-commit",
                &head,
            ],
        )
        .await?;
    }
    let merged = gh(
        i,
        &[
            "pr",
            "view",
            &number,
            "--repo",
            &d.repository,
            "--json",
            "state,mergeCommit,url",
        ],
    )
    .await?;
    if merged["state"] != "MERGED" {
        bail!("merge has not completed");
    }
    save(i, "merge", "succeeded", &merged)?;
    if let Some(workflow) = &d.deploy_workflow {
        let sha = merged["mergeCommit"]["oid"]
            .as_str()
            .context("merge commit")?;
        // Observe a configured push-triggered deployment, avoiding duplicate workflow_dispatch writes.
        let mut run = None;
        for _ in 0..30 {
            let runs = gh(
                i,
                &[
                    "run",
                    "list",
                    "--repo",
                    &d.repository,
                    "--workflow",
                    workflow,
                    "--commit",
                    sha,
                    "--json",
                    "databaseId,headSha,status,conclusion",
                    "--limit",
                    "10",
                ],
            )
            .await?;
            if let Some(found) = runs.as_array().and_then(|a| a.first()) {
                run = Some(found.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let run = run.context(
            "no deployment run found for merge commit; configure a push-triggered workflow",
        )?;
        save(i, "deployment", "observing", &run)?;
        gh(
            i,
            &[
                "run",
                "watch",
                &run["databaseId"].to_string(),
                "--repo",
                &d.repository,
                "--exit-status",
            ],
        )
        .await?;
        save(i, "deployment", "succeeded", &run)?;
    }
    if let Some(url) = &d.health_url {
        if let Err(error) = check_health(url, 5, std::time::Duration::from_secs(2)).await {
            save(
                i,
                "health",
                "failed",
                &json!({"url":url,"error":error.to_string()}),
            )?;
            return Err(error);
        }
        save(i, "health", "succeeded", &json!({"url":url}))?;
    }
    Ok(json!({"result":merged["url"],"accepted":true,"delivery":"complete"}))
}

pub async fn check_health(url: &str, attempts: usize, delay: std::time::Duration) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    for index in 0..attempts {
        if client
            .get(url)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return Ok(());
        }
        if index + 1 < attempts {
            tokio::time::sleep(delay).await;
        }
    }
    bail!("deployment health check failed: {url}")
}
