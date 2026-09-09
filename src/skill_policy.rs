//! Reviewed project skill overrides, separate from immutable shipped baselines.
use crate::{
    skills::{self, Bundle, Packet},
    store::{Store, id, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn project_path(repo: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(repo).context("project directory does not exist")?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "project must be a directory, not a symlink"
    );
    repo.canonicalize().context("resolve project directory")
}
fn project(args: &Value) -> Result<PathBuf> {
    ensure!(
        args.get("scope").is_none_or(|value| value == "project"),
        "only project-scoped skill overrides are supported"
    );
    project_path(Path::new(
        args["repo"].as_str().context("repo is required")?,
    ))
}
fn name(args: &Value) -> Result<&str> {
    let name = args["name"].as_str().context("skill name is required")?;
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "invalid skill name"
    );
    Ok(name)
}
fn instructions(bundle: &Bundle) -> Result<String> {
    Ok(String::from_utf8(hex::decode(
        &bundle
            .files
            .get("SKILL.md")
            .context("skill requires SKILL.md")?
            .hex,
    )?)?)
}
fn decode(name: &str, text: &str) -> Result<Bundle> {
    let bundle: Bundle = serde_json::from_str(text)?;
    skills::validate(&BTreeMap::from([(name.to_owned(), bundle.clone())]))?;
    Ok(bundle)
}
fn head(db: &Store, repo: &Path, name: &str) -> Result<(i64, Option<Bundle>)> {
    let row: Option<(i64, Option<String>)> = db
        .conn
        .query_row(
            "SELECT revision,bundle FROM skill_policy_heads WHERE repo=? AND name=?",
            params![repo.to_str().context("project path")?, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (revision, bundle) = row.unwrap_or((0, None));
    Ok((
        revision,
        bundle.map(|text| decode(name, &text)).transpose()?,
    ))
}

pub(crate) fn effective(db: &Store, repo: &Path, baseline: Packet) -> Result<Packet> {
    let repo = project_path(repo)?;
    let packet: Packet = baseline
        .into_iter()
        .map(|(name, bundle)| {
            let (_, overridden) = head(db, &repo, &name)?;
            Ok((name, overridden.unwrap_or(bundle)))
        })
        .collect::<Result<_>>()?;
    skills::validate(&packet)?;
    Ok(packet)
}

struct Snapshot {
    repo: PathBuf,
    name: String,
    baseline: Bundle,
    effective: Bundle,
    revision: i64,
    source: &'static str,
    overridden: bool,
}
impl Snapshot {
    fn load(db: &Store, args: &Value) -> Result<Self> {
        let repo = project(args)?;
        let name = name(args)?.to_owned();
        let configured = crate::config::Settings::load(&repo)?.skills;
        let baseline = skills::baseline_for_root(&db.root, &repo, &configured)?
            .remove(&name)
            .context("unknown skill; inspect the available project skills")?;
        let (revision, overridden) = head(db, &repo, &name)?;
        let source = if configured.contains_key(&name) {
            "configured"
        } else {
            "builtin"
        };
        Ok(Self {
            repo,
            name,
            baseline: baseline.clone(),
            effective: overridden.clone().unwrap_or(baseline),
            revision,
            source,
            overridden: overridden.is_some(),
        })
    }
    fn view(&self) -> Result<Value> {
        Ok(
            json!({"scope":"project","repo":self.repo,"name":self.name,"source":self.source,"baseline_hash":self.baseline.hash,"effective_hash":self.effective.hash,"revision":self.revision,"overridden":self.overridden,"baseline_content":instructions(&self.baseline)?,"content":instructions(&self.effective)?}),
        )
    }
}

pub fn inspect(db: &Store, args: &Value) -> Result<Value> {
    if args.get("name").is_some() {
        return Snapshot::load(db, args)?.view();
    }
    let repo = project(args)?;
    let configured = crate::config::Settings::load(&repo)?.skills;
    let packet = skills::baseline_for_root(&db.root, &repo, &configured)?;
    let catalog = packet.into_iter().map(|(name,baseline)| {
        let (revision,overridden) = head(db,&repo,&name)?;
        Ok(json!({"name":name,"source":if configured.contains_key(&name){"configured"}else{"builtin"},"baseline_hash":baseline.hash,"effective_hash":overridden.as_ref().unwrap_or(&baseline).hash,"revision":revision,"overridden":overridden.is_some()}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(json!({"scope":"project","repo":repo,"skills":catalog}))
}

fn diff(name: &str, before: &str, after: &str) -> String {
    let removed = before
        .lines()
        .map(|line| format!("-{line}\n"))
        .collect::<String>();
    let added = after
        .lines()
        .map(|line| format!("+{line}\n"))
        .collect::<String>();
    format!(
        "--- effective/{name}/SKILL.md\n+++ proposed/{name}/SKILL.md\n@@ -1,{} +1,{} @@\n{removed}{added}",
        before.lines().count(),
        after.lines().count()
    )
}

fn changed_files(before: &Bundle, after: &Bundle) -> Vec<Value> {
    let paths: std::collections::BTreeSet<_> =
        before.files.keys().chain(after.files.keys()).collect();
    paths.into_iter().filter_map(|path| {
        let old = before.files.get(path);
        let new = after.files.get(path);
        if old.map(|file| (&file.hex,file.executable)) == new.map(|file| (&file.hex,file.executable)) { return None; }
        let describe = |file: &skills::File| json!({"sha256":crate::store::hash(&hex::decode(&file.hex).expect("validated skill encoding")),"bytes":file.hex.len()/2,"executable":file.executable});
        Some(json!({"path":path,"before":old.map(describe),"after":new.map(describe)}))
    }).collect()
}

fn propose_bundle(
    db: &Store,
    snapshot: &Snapshot,
    bundle: &Bundle,
    reset: bool,
    reason: &str,
) -> Result<Value> {
    ensure!(reason.len() <= 4096, "skill proposal reason exceeds 4 KiB");
    skills::validate(&BTreeMap::from([(snapshot.name.clone(), bundle.clone())]))?;
    let proposal_id = id();
    db.conn.execute(
        "INSERT INTO skill_policy_proposals VALUES(?,?,?,?,?,?,?,?,?,'proposed',?,NULL)",
        params![
            proposal_id,
            snapshot.repo.to_str().context("project path")?,
            snapshot.name,
            snapshot.effective.hash,
            snapshot.revision,
            snapshot.baseline.hash,
            serde_json::to_string(bundle)?,
            reset,
            reason,
            now()
        ],
    )?;
    crate::management::event(
        db,
        "skill.proposed",
        json!({"proposal_id":proposal_id,"repo":snapshot.repo,"name":snapshot.name,"base_hash":snapshot.effective.hash,"proposed_hash":bundle.hash}),
    )?;
    Ok(
        json!({"proposal_id":proposal_id,"scope":"project","repo":snapshot.repo,"name":snapshot.name,"base_hash":snapshot.effective.hash,"baseline_hash":snapshot.baseline.hash,"proposed_hash":bundle.hash,"base_revision":snapshot.revision,"content":instructions(bundle)?,"changed_files":changed_files(&snapshot.effective,bundle),"diff":diff(&snapshot.name,&instructions(&snapshot.effective)?,&instructions(bundle)?),"reason":reason,"reset_to_baseline":reset,"requires_acceptance":true,"state":"proposed"}),
    )
}

pub fn propose(db: &Store, args: &Value) -> Result<Value> {
    db.atomic(|| {
        let snapshot = Snapshot::load(db, args)?;
        if let Some(expected) = args.get("expected_hash") {
            ensure!(
                expected.as_str() == Some(snapshot.effective.hash.as_str()),
                "effective skill changed; inspect it and propose again"
            );
        }
        let content = args["content"]
            .as_str()
            .context("skill content is required")?;
        let bundle = skills::with_instructions(&snapshot.effective, content)?;
        propose_bundle(
            db,
            &snapshot,
            &bundle,
            false,
            args["reason"].as_str().unwrap_or(""),
        )
    })
}

struct Proposal {
    name: String,
    base_hash: String,
    base_revision: i64,
    baseline_hash: String,
    bundle: Bundle,
    reset: bool,
    reason: String,
    state: String,
    applied_revision: Option<i64>,
}
fn proposal(db: &Store, repo: &Path, id: &str) -> Result<Proposal> {
    let row = db
        .rows(
            "SELECT * FROM skill_policy_proposals WHERE id=? AND repo=?",
            &[&id, &repo.to_str().context("project path")?],
        )?
        .into_iter()
        .next()
        .context("unknown proposal for this project")?;
    let name = row["name"].as_str().context("proposal name")?.to_owned();
    Ok(Proposal {
        bundle: decode(&name, row["bundle"].as_str().context("proposal bundle")?)?,
        name,
        base_hash: row["base_hash"].as_str().context("base hash")?.into(),
        base_revision: row["base_revision"].as_i64().context("base revision")?,
        baseline_hash: row["baseline_hash"]
            .as_str()
            .context("baseline hash")?
            .into(),
        reset: row["reset"].as_i64() == Some(1),
        reason: row["reason"].as_str().context("proposal reason")?.into(),
        state: row["state"].as_str().context("proposal state")?.into(),
        applied_revision: row["applied_revision"].as_i64(),
    })
}

pub fn apply(db: &Store, args: &Value) -> Result<Value> {
    ensure!(
        crate::branding::var_os("HORDE_WORKER_TOKEN").is_none(),
        "persistent skill changes require caller approval"
    );
    ensure!(
        args["accepted"] == true,
        "review and explicitly accept the proposal before applying it"
    );
    let repo = project(args)?;
    let proposal_id = args["proposal_id"]
        .as_str()
        .context("proposal_id is required")?;
    db.atomic(|| {
        let proposed = proposal(db,&repo,proposal_id)?;
        if proposed.state == "applied" {
            return Ok(json!({"proposal_id":proposal_id,"repo":repo,"name":proposed.name,"revision":proposed.applied_revision,"applied_hash":proposed.bundle.hash,"state":"applied","already_applied":true}));
        }
        ensure!(proposed.state == "proposed", "proposal is not applicable");
        let current = Snapshot::load(db,&json!({"repo":repo,"name":proposed.name}))?;
        ensure!(current.effective.hash == proposed.base_hash && current.revision == proposed.base_revision && current.baseline.hash == proposed.baseline_hash, "skill or baseline changed since proposal; inspect it and propose again");
        let revision = current.revision + 1;
        let serialized = serde_json::to_string(&proposed.bundle)?;
        db.conn.execute("INSERT INTO skill_policy_heads VALUES(?,?,?,?) ON CONFLICT(repo,name) DO UPDATE SET revision=excluded.revision,bundle=excluded.bundle",params![repo.to_str().context("project path")?,proposed.name,revision,if proposed.reset {None}else{Some(serialized.as_str())}])?;
        db.conn.execute("INSERT INTO skill_policy_revisions VALUES(?,?,?,?,?,?,?,?)",params![repo.to_str().context("project path")?,proposed.name,revision,serialized,proposed.reset,proposal_id,proposed.reason,now()])?;
        db.conn.execute("UPDATE skill_policy_proposals SET state='applied',applied_revision=? WHERE id=?",params![revision,proposal_id])?;
        crate::management::event(db,"skill.applied",json!({"proposal_id":proposal_id,"repo":repo,"name":proposed.name,"revision":revision,"hash":proposed.bundle.hash}))?;
        Ok(json!({"proposal_id":proposal_id,"repo":repo,"name":proposed.name,"revision":revision,"applied_hash":proposed.bundle.hash,"state":"applied","already_applied":false}))
    })
}

pub fn history(db: &Store, args: &Value) -> Result<Value> {
    let repo = project(args)?;
    let name = name(args)?;
    let limit = args
        .get("limit")
        .map(|value| value.as_i64().context("history limit must be an integer"))
        .transpose()?
        .unwrap_or(25);
    ensure!((1..=100).contains(&limit), "history limit must be 1..100");
    let before = args
        .get("before_revision")
        .map(|value| value.as_i64().context("before_revision must be an integer"))
        .transpose()?
        .unwrap_or(i64::MAX);
    ensure!(before > 0, "before_revision must be positive");
    let rows = db.rows("SELECT revision,json_extract(bundle,'$.hash') AS hash,reset,proposal_id,reason,created FROM skill_policy_revisions WHERE repo=? AND name=? AND revision<? ORDER BY revision DESC LIMIT ?", &[&repo.to_str().context("project path")?,&name,&before,&limit])?;
    let next_before_revision = if rows.len() == limit as usize {
        rows.last().and_then(|row| row["revision"].as_i64())
    } else {
        None
    };
    let revisions = rows.into_iter().map(|row| {
        let hash = row["hash"].as_str().context("history bundle hash")?;
        ensure!(hash.len() == 64 && hash.bytes().all(|byte|byte.is_ascii_hexdigit()), "invalid history bundle hash");
        Ok(json!({"revision":row["revision"],"hash":hash,"reset_to_baseline":row["reset"]==1,"proposal_id":row["proposal_id"],"reason":row["reason"],"created":row["created"]}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"repo":repo,"name":name,"scope":"project","revisions":revisions,"next_before_revision":next_before_revision}),
    )
}

/// Rollback is a proposal, so restoring prior guidance uses the same acceptance gate.
pub fn rollback(db: &Store, args: &Value) -> Result<Value> {
    let revision = args["revision"].as_i64().context("revision is required")?;
    ensure!(revision >= 0, "revision must be nonnegative");
    db.atomic(|| {
        let snapshot = Snapshot::load(db,args)?;
        let target = if revision == 0 { snapshot.baseline.clone() } else {
            let saved: String = db.conn.query_row("SELECT bundle FROM skill_policy_revisions WHERE repo=? AND name=? AND revision=?",params![snapshot.repo.to_str().context("project path")?,snapshot.name,revision],|r|r.get(0)).optional()?.context("unknown skill revision")?;
            decode(&snapshot.name,&saved)?
        };
        let default_reason = format!("Restore skill revision {revision}");
        propose_bundle(db,&snapshot,&target,revision==0,args["reason"].as_str().unwrap_or(&default_reason))
    })
}
