use super::*;

pub(super) fn non_delivery_ops(db: &Store, task: &str) -> Result<String> {
    let rows = db.rows(
        "SELECT name,state,data FROM external_ops WHERE task=? ORDER BY name",
        &[&task],
    )?;
    let filtered = rows
        .into_iter()
        .filter(|v| !DELIVERY_NAMES.contains(&v["name"].as_str().unwrap_or("")))
        .collect::<Vec<_>>();
    json_hash(&json!(filtered))
}

pub(super) fn revision_and_context(i: &Invocation<'_>) -> Result<(i64, i64)> {
    let revision = crate::decision::review::current_revision(i.db, i.task)?;
    let context = crate::delegation::mandatory(i.db, i.task)?["version"]
        .as_i64()
        .context("context version")?;
    Ok((revision, context))
}

pub(super) fn review_eligible(row: &Value, head: &str, revision: i64, context: i64) -> bool {
    row["state"] == "succeeded"
        && row["fresh"] == true
        && row["head"] == head
        && row["revision"] == revision
        && row["context_version"] == context
        && row["coverage"]["complete"] == true
}

pub(super) fn review_signals_clear(result: &Value, minimum_confidence: f64) -> bool {
    let Some(answers) = result["answers"].as_array() else {
        return false;
    };
    let mut specialist = false;
    let mut signals = 0;
    for answer in answers {
        if answer["id"] == "specialist" {
            specialist = answer["answer"] == "none"
                && answer["confidence"]
                    .as_f64()
                    .is_some_and(|v| v >= minimum_confidence);
        } else if answer["type"] == "noul" {
            let Some(value) = answer["noul"].as_f64() else {
                return false;
            };
            if value > 0.5
                || !value.is_finite()
                || !answer["confidence"]
                    .as_f64()
                    .is_some_and(|v| v >= minimum_confidence)
            {
                return false;
            }
            signals += 1;
        }
    }
    specialist && signals == 6
}

pub(super) fn reviewer_result_passes(result: &Value, head: &str, revision: i64) -> bool {
    result["accepted"] == true
        && result["verdict"] == "pass"
        && result["risk"] == "routine"
        && result["coverage_complete"] == true
        && result["findings"].as_array().is_some_and(Vec::is_empty)
        && result["reviewed_head"] == head
        && result["reviewed_revision"] == revision
}

pub(super) fn independent_review(i: &Invocation<'_>, head: &str, revision: i64) -> Result<Value> {
    let latest_integration: i64 = i.db.conn.query_row(
        "SELECT COALESCE(MAX(created),0) FROM integrations WHERE task=? AND state='succeeded'",
        [i.task],
        |r| r.get(0),
    )?;
    let integrators =
        i.db.rows(
            "SELECT DISTINCT worker FROM integrations WHERE task=? AND state='succeeded'",
            &[&i.task],
        )?
        .into_iter()
        .filter_map(|v| v["worker"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let rows = i.db.rows(
        "SELECT a.id,a.worker,a.state,a.started,a.result,s.spec FROM attempts a
        JOIN steps s ON s.id=a.step WHERE s.task=? ORDER BY a.finished DESC,a.started DESC",
        &[&i.task],
    )?;
    for row in rows {
        let Some(worker) = row["worker"].as_str() else {
            continue;
        };
        if integrators.contains(worker) || row["started"].as_i64().unwrap_or(0) < latest_integration
        {
            continue;
        }
        let Some(spec) = row["spec"]
            .as_str()
            .and_then(|v| serde_json::from_str::<crate::template::Step>(v).ok())
        else {
            continue;
        };
        if spec.kind != "agent" || spec.role != "reviewer" {
            continue;
        }
        for (name, kind) in [
            ("reviewed_head", "string"),
            ("reviewed_revision", "integer"),
            ("verdict", "string"),
            ("risk", "string"),
            ("coverage_complete", "boolean"),
            ("findings", "array"),
        ] {
            ensure!(
                spec.output_types.get(name).is_some_and(|v| v == kind),
                "reviewer step must declare structured delivery evidence: {name}"
            );
        }
        let result: Value = row["result"]
            .as_str()
            .and_then(|v| serde_json::from_str(v).ok())
            .context("latest reviewer result is unavailable")?;
        ensure!(
            row["state"] == "succeeded" && reviewer_result_passes(&result, head, revision),
            "latest independent reviewer did not pass the exact head and revision"
        );
        return Ok(
            json!({"attempt":row["id"],"worker":worker,"result_hash":json_hash(&result)?,
            "reviewed_head":head,"reviewed_revision":revision}),
        );
    }
    bail!(
        "automatic delivery needs an independent passing reviewer result bound to the exact head and revision"
    )
}

pub(super) fn review_snapshot(
    i: &Invocation<'_>,
    head: &str,
    revision: i64,
    context: i64,
    minimum_confidence: f64,
) -> Result<Value> {
    let mut cursor = 0;
    let mut selected = None;
    loop {
        let rows = crate::decision::review::list(i.db, i.task, cursor, 200)?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            cursor = row["event_seq"].as_i64().context("review event sequence")?;
            if review_eligible(row, head, revision, context) {
                ensure!(
                    review_signals_clear(&row["result"], minimum_confidence),
                    "review checkpoint flagged an unresolved concern or uncertainty"
                );
                selected = Some(json!({"id":row["id"],"head":head,"revision":revision,
                    "context_version":context,"evidence_hash":row["evidence_hash"],
                    "manifest_hash":row["manifest_hash"],"external_hash":row["external_hash"],
                    "signals_hash":json_hash(&row["result"])?}));
            }
        }
        if rows.len() < 200 {
            break;
        }
    }
    selected.context(
        "automatic delivery needs a fresh, complete checkpoint with independent generative review",
    )
}

/// Capture review freshness before delivery itself changes external_ops. A restart
/// reuses this immutable snapshot and checks that non-delivery effects did not drift.
pub fn preflight(i: &Invocation<'_>, head: &str) -> Result<()> {
    if !i.settings.delivery.merge {
        return Ok(());
    }
    let Some(policy) = current_policy(i)? else {
        return Ok(());
    };
    scoped(i, &policy)?;
    let (revision, context) = revision_and_context(i)?;
    let policy_hash = json_hash(&serde_json::to_value(&policy)?)?;
    let external_hash = non_delivery_ops(i.db, i.task)?;
    let existing: Option<String> =
        i.db.conn
            .query_row(
                "SELECT data FROM delivery_preflights WHERE task=?",
                [i.task],
                |r| r.get(0),
            )
            .optional()?;
    if let Some(raw) = existing {
        let snapshot: Value = serde_json::from_str(&raw)?;
        ensure!(
            snapshot["head"] == head
                && snapshot["revision"] == revision
                && snapshot["context_version"] == context
                && snapshot["policy_hash"] == policy_hash
                && snapshot["non_delivery_ops_hash"] == external_hash,
            "automatic-delivery preflight is stale; reconcile before retry"
        );
        return Ok(());
    }
    let review = review_snapshot(i, head, revision, context, policy.minimum_review_confidence)?;
    let independent = independent_review(i, head, revision)?;
    let snapshot = json!({"head":head,"revision":revision,"context_version":context,
        "policy_hash":policy_hash,"non_delivery_ops_hash":external_hash,"review":review,
        "independent_review":independent});
    i.db.conn.execute(
        "INSERT INTO delivery_preflights(task,data,created) VALUES(?,?,?)",
        params![i.task, snapshot.to_string(), now()],
    )?;
    Ok(())
}

pub(super) fn path_is_sensitive(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let parts = lower.split('/').collect::<Vec<_>>();
    parts.iter().any(|part| {
        [
            "security",
            "auth",
            "migrations",
            "migration",
            "schema",
            "secrets",
            "credentials",
            "api",
            "openapi",
            ".github",
            ".gitlab",
            "workflows",
            "ci",
            "deploy",
            "deployment",
            "config",
            "scripts",
        ]
        .contains(part)
    }) || lower.ends_with(".sql")
        || lower.ends_with(".proto")
        || [
            "dockerfile",
            "makefile",
            "cargo.toml",
            "package.json",
            "build.rs",
        ]
        .contains(&parts.last().copied().unwrap_or(""))
}

pub(super) fn routine_manifest(
    workspace: &Path,
    base: &str,
    head: &str,
) -> Result<(Vec<String>, Value)> {
    let base_ref = format!("refs/remotes/origin/{base}");
    let manifest = crate::decision::review::delivery_manifest(workspace, &base_ref, head)?;
    ensure!(
        manifest["complete"] == true,
        "automatic delivery needs complete diff coverage"
    );
    let rows = manifest["paths"].as_array().context("manifest paths")?;
    ensure!(
        !rows.is_empty() && rows.len() <= 8,
        "automatic delivery requires 1..=8 covered paths"
    );
    let mut paths = Vec::new();
    for row in rows {
        let status = row["status"].as_str().context("change status")?;
        ensure!(
            status == "A" || status == "M",
            "rename, deletion, or type change requires manual delivery"
        );
        paths.push(row["path"].as_str().context("changed path")?.to_owned());
    }
    for excerpt in manifest["excerpts"].as_array().context("diff excerpts")? {
        let content = excerpt["excerpt"]
            .as_str()
            .context("missing diff excerpt")?;
        ensure!(
            !diff_has_sensitive_change(content),
            "diff changes a potential public API or security boundary"
        );
    }
    Ok((paths, manifest))
}

pub(super) fn diff_has_sensitive_change(excerpt: &str) -> bool {
    excerpt
        .lines()
        .filter(|line| {
            (line.starts_with('+') && !line.starts_with("+++"))
                || (line.starts_with('-') && !line.starts_with("---"))
        })
        .map(|line| &line[1..])
        .map(|line| line.trim().to_ascii_lowercase())
        .any(|line| {
            [
                "pub ",
                "pub(",
                "export ",
                ".route(",
                "router",
                "authorization",
                "authentication",
                "permission",
                "password",
                "secret",
                "token",
                "credential",
                "migration",
                "schema",
                "create table",
                "alter table",
                "drop table",
                "openapi",
            ]
            .iter()
            .any(|needle| line.contains(needle))
        })
}

pub(super) fn redacted_manifest(i: &Invocation<'_>, manifest: &Value) -> Result<Value> {
    let mut values = crate::secrets::values(i.db, i.task)?;
    let credential = crate::config::credential(&i.settings.decision.api_key_env)?;
    ensure!(!credential.is_empty(), "decision credential unavailable");
    values.insert("__delivery_credential".into(), credential);
    for value in values.values().cloned().collect::<Vec<_>>() {
        let encoded = serde_json::to_string(&value)?;
        if let Some(inner) = encoded.get(1..encoded.len().saturating_sub(1))
            && inner != value
        {
            values.insert(format!("__escaped_{}", values.len()), inner.to_owned());
        }
    }
    Ok(crate::secrets::redact_json(manifest, &values))
}

pub(super) fn validate_paths(paths: &[String], allowed: &[String]) -> Result<()> {
    for path in paths {
        ensure!(
            !path_is_sensitive(path),
            "sensitive change requires manual delivery: {path}"
        );
        ensure!(
            allowed
                .iter()
                .any(|prefix| path == prefix || path.starts_with(&format!("{prefix}/"))),
            "changed path is outside automatic-delivery scope: {path}"
        );
    }
    Ok(())
}
