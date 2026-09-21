use horde::{
    config::{AutomaticDelivery, Decision, DecisionMode, Delivery, Settings},
    executor::Invocation,
    store::{Store, hash, now},
    template::{Plan, Step},
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn git(repo: &Path, args: &[&str]) -> String {
    horde::git::run(repo, args).unwrap()
}

async fn http_server(bodies: Vec<Value>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for body in bodies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await.unwrap();
            let payload = body.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).as_bytes()).await.unwrap();
        }
    });
    format!("http://{address}")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Success,
    LostMerge,
    FailedCheck,
    BaseRace,
    CancelAtIntent,
    WrongPrOnRestart,
}

fn fake_gh(path: &Path, ledger: &Path, base: &str, head: &str, merge: &str, scenario: Scenario) {
    let script = r#"#!/usr/bin/env python3
import json, pathlib, sys
ledger=pathlib.Path(__LEDGER__)
args=sys.argv[1:]
with (ledger.with_suffix('.calls')).open('a') as out: out.write(' '.join(args)+'\n')
base=__BASE__; head=__HEAD__; merge=__MERGE__; scenario=__SCENARIO__
created=ledger.with_suffix('.created'); merged=ledger.with_suffix('.merged')
def respond(value): print(json.dumps(value)); sys.exit(0)
if args[:2]==['repo','view']: respond({'nameWithOwner':'test/repo'})
if args[:2]==['pr','list']:
    number=2 if scenario=='wrong_pr' and merged.exists() else 1
    respond([] if not created.exists() else [{'number':number,'url':'https://example.invalid/pr/'+str(number),'state':'MERGED' if merged.exists() else 'OPEN','headRefOid':head}])
if args[:2]==['pr','create']:
    created.write_text('yes'); respond('https://example.invalid/pr/1')
if args[:2]==['pr','checks']: respond('checks passed')
if args[:2]==['pr','view']:
    number=2 if scenario=='wrong_pr' and merged.exists() else 1
    respond({'number':number,'url':'https://example.invalid/pr/'+str(number),'state':'MERGED' if merged.exists() else 'OPEN',
        'headRefOid':head,'baseRefName':'main','author':{'login':'author'},'mergeCommit':{'oid':merge}})
if args[:2]==['pr','merge']:
    merged.write_text('yes')
    if scenario=='lost': print('lost merge response',file=sys.stderr); sys.exit(1)
    respond('merged')
if args and args[0]=='api':
    endpoint=args[1]
    if endpoint.endswith('/branches/main/protection'):
        respond({'required_status_checks':{'strict':True,'contexts':['test']},'enforce_admins':{'enabled':True},
            'required_pull_request_reviews':{'required_approving_review_count':1,
                'bypass_pull_request_allowances':{'users':[],'teams':[],'apps':[]}}})
    if endpoint.endswith('/branches/main'):
        base_calls=ledger.with_suffix('.base_calls')
        count=int(base_calls.read_text())+1 if base_calls.exists() else 1
        base_calls.write_text(str(count))
        respond({'commit':{'sha':'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' if scenario=='base_race' and count>1 else base}})
    if '/check-runs?' in endpoint:
        respond({'total_count':1,'check_runs':[{'id':7,'name':'test','head_sha':head,
            'status':'completed','conclusion':'failure' if scenario=='failed_check' else 'success',
            'completed_at':'2026-09-21T00:00:00Z'}]})
    if '/pulls/1/reviews?' in endpoint:
        respond([{'id':8,'submitted_at':'2026-09-21T00:00:00Z','user':{'login':'reviewer'},'state':'APPROVED','commit_id':head}])
    if endpoint.endswith('/commits/'+merge): respond({'parents':[{'sha':base}]})
if args[:2]==['run','list']:
    respond([{'databaseId':9,'headSha':merge,'headBranch':'main','event':'push','status':'completed','conclusion':'success'}])
if args[:2]==['run','watch']: respond('done')
if args[:2]==['run','view']:
    respond({'databaseId':9,'headSha':merge,'headBranch':'main','event':'push','status':'completed','conclusion':'success'})
print('unexpected command',args,file=sys.stderr); sys.exit(3)
"#;
    let script = script
        .replace(
            "__LEDGER__",
            &serde_json::to_string(&ledger.to_string_lossy()).unwrap(),
        )
        .replace("__BASE__", &serde_json::to_string(base).unwrap())
        .replace("__HEAD__", &serde_json::to_string(head).unwrap())
        .replace("__MERGE__", &serde_json::to_string(merge).unwrap())
        .replace(
            "__SCENARIO__",
            &serde_json::to_string(match scenario {
                Scenario::Success => "success",
                Scenario::LostMerge => "lost",
                Scenario::FailedCheck => "failed_check",
                Scenario::BaseRace => "base_race",
                Scenario::CancelAtIntent => "cancel_at_intent",
                Scenario::WrongPrOnRestart => "wrong_pr",
            })
            .unwrap(),
        );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn reviewer_step() -> Step {
    serde_json::from_value(json!({"id":"review","kind":"agent","role":"reviewer",
        "output_types":{"reviewed_head":"string","reviewed_revision":"integer","verdict":"string",
            "risk":"string","coverage_complete":"boolean","findings":"array"}}))
    .unwrap()
}

fn signals() -> Value {
    let mut answers = [
        "requirements",
        "correctness",
        "security",
        "tests",
        "integration",
        "insufficient_evidence",
    ]
    .into_iter()
    .map(|id| json!({"type":"noul","id":id,"noul":0.1,"confidence":0.9}))
    .collect::<Vec<_>>();
    answers.push(json!({"type":"choice","id":"specialist","answer":"none","confidence":0.9,"probabilities":{"none":0.9}}));
    json!({"answers":answers,"usage":{}})
}

#[tokio::test]
async fn automatic_delivery_uses_trusted_gh_and_verifies_merge_deployment_and_smoke() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config")) };
    let result = run_case(dir.path(), Scenario::Success).await;
    if let Some(value) = prior {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", value) };
    } else {
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
    result.unwrap();
}

#[tokio::test]
async fn lost_merge_response_reconciles_without_duplicate_after_policy_is_disabled() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config")) };
    let result = run_case(dir.path(), Scenario::LostMerge).await;
    if let Some(value) = prior {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", value) };
    } else {
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
    result.unwrap();
}

#[tokio::test]
async fn a_different_merged_pr_cannot_reuse_prior_authorization() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config")) };
    let result = run_case(dir.path(), Scenario::WrongPrOnRestart).await;
    if let Some(value) = prior {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", value) };
    } else {
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
    result.unwrap();
}

#[tokio::test]
async fn failed_check_stops_before_merge() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config")) };
    let result = run_case(dir.path(), Scenario::FailedCheck).await;
    if let Some(value) = prior {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", value) };
    } else {
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
    result.unwrap();
}

#[tokio::test]
async fn changed_base_stops_before_merge() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config")) };
    let result = run_case(dir.path(), Scenario::BaseRace).await;
    if let Some(value) = prior {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", value) };
    } else {
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
    result.unwrap();
}

#[tokio::test]
async fn cancellation_after_merge_intent_stops_external_merge() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config")) };
    let result = run_case(dir.path(), Scenario::CancelAtIntent).await;
    if let Some(value) = prior {
        unsafe { std::env::set_var("XDG_CONFIG_HOME", value) };
    } else {
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }
    result.unwrap();
}

async fn run_case(root: &Path, scenario: Scenario) -> anyhow::Result<()> {
    let repo = root.join("repo");
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.test"]);
    git(&repo, &["commit", "--allow-empty", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let remote = root.join("remote.git");
    std::fs::create_dir(&remote)?;
    git(&remote, &["init", "--bare"]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let db = Store::open(&root.join("state"))?;
    let plan = Plan {
        warnings: vec![],
        steps: vec![
            reviewer_step(),
            serde_json::from_value(
                json!({"id":"deliver","kind":"delivery","instructions":"Ship routine UI change"}),
            )?,
        ],
        pins: BTreeMap::new(),
        outputs: BTreeMap::new(),
    };
    let task = db.submit(
        "test automatic delivery",
        &repo,
        &Settings::default(),
        &plan,
    )?;
    let workspace = db.root.join("workspaces").join(&task).join("integrated");
    std::fs::create_dir_all(workspace.parent().unwrap())?;
    git(
        root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            workspace.to_str().unwrap(),
        ],
    );
    git(&workspace, &["config", "user.name", "Test"]);
    git(&workspace, &["config", "user.email", "test@example.test"]);
    std::fs::create_dir_all(workspace.join("src/ui"))?;
    std::fs::write(workspace.join("src/ui/button.rs"), "text = new;\n")?;
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-qm", "routine change"]);
    let head = git(&workspace, &["rev-parse", "HEAD"]);
    let merge = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let gh_path = root.join("trusted-gh");
    let ledger = root.join("github");
    fake_gh(&gh_path, &ledger, &base, &head, merge, scenario);
    let assertion_url = http_server(vec![
        json!({"commit":merge,"environment":"staging"}),
        json!({"ok":true,"commit":merge,"environment":"staging"}),
        json!({"commit":merge,"environment":"staging"}),
        json!({"ok":true,"commit":merge,"environment":"staging"}),
    ])
    .await;
    let jev_response = r#"{"model":"jev-1.13.0","answers":{"delivery":{"type":"choice","choice":"routine","confidence":0.95,"probabilities":{"routine":0.95,"escalate":0.05}}}}"#;
    let (jev_url, mut jev_requests) = support::server(vec![("200 OK", jev_response, 0)]).await;
    let key_name = "HORDE_DELIVERY_TEST_KEY";
    unsafe { std::env::set_var(key_name, "test-key") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        review_enabled: true,
        backend: "tuara".into(),
        base_url: jev_url,
        api_key_env: key_name.into(),
        ..Decision::default()
    };
    let policy = AutomaticDelivery {
        enabled: true,
        gh_program: gh_path.to_str().unwrap().into(),
        repositories: vec!["test/repo".into()],
        bases: vec!["main".into()],
        environments: vec!["staging".into()],
        deploy_workflows: vec!["deploy.yml".into()],
        allowed_paths: vec!["src/ui".into()],
        required_checks: vec!["test".into()],
        minimum_approvals: 1,
        require_independent_review: true,
        version_url: Some(assertion_url.clone()),
        smoke_url: Some(assertion_url),
        qualification_id: "evaluation-1".into(),
        minimum_routine_probability: 0.8,
        minimum_review_confidence: 0.8,
    };
    let settings = Settings {
        delivery: Delivery {
            enabled: true,
            repository: "test/repo".into(),
            base: "main".into(),
            merge: true,
            deploy_workflow: Some("deploy.yml".into()),
            environment: Some("staging".into()),
            program: Some("/does/not/exist".into()),
            ..Default::default()
        },
        decision: decision.clone(),
        automatic_delivery: policy.clone(),
        ..Settings::default()
    };
    let config_dir = root.join("config/horde");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::write(
        config_dir.join("config.toml"),
        toml::to_string(&Settings {
            decision: decision.clone(),
            automatic_delivery: policy.clone(),
            ..Settings::default()
        })?,
    )?;
    let policy_hash = hash(&serde_json::to_vec(&serde_json::to_value(&policy)?)?);
    let report = json!({"backend":decision.backend,"model":decision.model,"policy":decision.policy,"backend_fingerprint":decision.fingerprint()?,
        "purpose":"delivery:veto","catalog":"delivery-v1","delivery_policy_hash":policy_hash,
        "minimum_routine_probability":0.8,"held_out":{"cases":50,"baseline_quality":0.9,"candidate_quality":0.91,
        "baseline_critical_defect_recall":1.0,"candidate_critical_defect_recall":1.0,
        "baseline_verified_completion_ms":1000,"candidate_verified_completion_ms":900}});
    let bytes = serde_json::to_vec(&report)?;
    let evidence = hash(&bytes);
    std::fs::create_dir_all(db.root.join("artifacts"))?;
    std::fs::write(db.root.join("artifacts").join(&evidence), &bytes)?;
    db.conn.execute(
        "INSERT INTO artifacts(hash,size,created) VALUES(?,?,?)",
        params![evidence, bytes.len() as i64, now()],
    )?;
    db.conn.execute("INSERT INTO delivery_qualifications(id,backend,model,policy,purpose,catalog,delivery_policy_hash,threshold,evidence_hash,held_out_passed,created) VALUES(?,?,?,?,?,?,?,?,?,1,?)",
        params!["evaluation-1",decision.backend,decision.model,decision.policy,"delivery:veto","delivery-v1",policy_hash,0.8,evidence,now()])?;
    let steps = db.steps(&task)?;
    let review_step_id = steps[0]["id"].as_str().unwrap();
    let delivery_step_id = steps[1]["id"].as_str().unwrap();
    db.conn.execute(
        "UPDATE steps SET state='running' WHERE id=?",
        [delivery_step_id],
    )?;
    let review_worker = "review-worker";
    let delivery_worker = "delivery-worker";
    for (worker, step) in [
        (review_worker, review_step_id),
        (delivery_worker, delivery_step_id),
    ] {
        db.conn.execute("INSERT INTO workers(id,task,step,status,token_hash,updated) VALUES(?,?,?,'active','test',?)",params![worker,task,step,now()])?;
    }
    let review_result = json!({"result":"reviewed","accepted":true,"artifacts":[],"verdict":"pass","risk":"routine",
        "coverage_complete":true,"findings":[],"reviewed_head":head,"reviewed_revision":1});
    db.conn.execute("INSERT INTO attempts(id,step,worker,state,started,finished,result) VALUES('review-attempt',?,?,'succeeded',?,?,?)",
        params![review_step_id,review_worker,now(),now(),review_result.to_string()])?;
    db.conn.execute("INSERT INTO attempts(id,step,worker,state,started) VALUES('delivery-attempt',?,?,'running',?)",
        params![delivery_step_id,delivery_worker,now()])?;
    if scenario == Scenario::CancelAtIntent {
        db.conn.execute_batch(
            "CREATE TRIGGER cancel_delivery_at_merge_intent
            AFTER INSERT ON external_ops
            WHEN NEW.name='merge' AND NEW.state='intent'
            BEGIN UPDATE tasks SET status='cancelled' WHERE id=NEW.task; END;",
        )?;
    }
    let review = signals();
    db.conn.execute("INSERT INTO decisions(id,task,purpose,state,policy,backend,model,queued,result) VALUES('review-decision',?,'review:integration','succeeded','review-v1','tuara','jev-1.13.0',?,?)",
        params![task,now(),review.to_string()])?;
    let external_hash = hash(b"[]");
    let event_seq: i64 =
        db.conn
            .query_row("SELECT MIN(seq) FROM events WHERE task=?", [&task], |row| {
                row.get(0)
            })?;
    db.conn.execute("INSERT INTO review_checkpoints(id,event_seq,task,kind,revision,context_version,base,head,external_hash,manifest_hash,evidence_hash,coverage,generative_review_attempts,created)
        VALUES('review-decision',?,?,'integration',1,1,?,?,?,?,?,?,?,?)",
        params![event_seq,task,base,head,external_hash,"manifest-hash","evidence-hash",json!({"complete":true}).to_string(),json!([]).to_string(),now()])?;
    let delivery_spec: Step = serde_json::from_str(steps[1]["spec"].as_str().unwrap())?;
    let invocation = Invocation {
        db: &db,
        task: &task,
        step: delivery_step_id,
        attempt: "delivery-attempt",
        worker: delivery_worker,
        token: "test",
        workspace: &workspace,
        spec: &delivery_spec,
        settings: &settings,
        context: json!({}),
    };
    let first = horde::delivery::execute(&invocation).await;
    if matches!(
        scenario,
        Scenario::FailedCheck | Scenario::BaseRace | Scenario::CancelAtIntent
    ) {
        assert!(first.is_err());
        let calls = std::fs::read_to_string(ledger.with_extension("calls"))?;
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.starts_with("pr merge"))
                .count(),
            0
        );
        unsafe { std::env::remove_var(key_name) };
        return Ok(());
    }
    if scenario == Scenario::LostMerge {
        assert!(first.is_err());
    } else {
        assert_eq!(first?["delivery"], "complete");
    }
    assert!(jev_requests.recv().await.is_some());
    std::fs::write(
        config_dir.join("config.toml"),
        toml::to_string(&Settings::default())?,
    )?;
    let result = horde::delivery::execute(&invocation).await;
    if scenario == Scenario::WrongPrOnRestart {
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("merge intent PR or head changed")
        );
    } else {
        assert_eq!(result?["delivery"], "complete");
    }
    let calls = std::fs::read_to_string(ledger.with_extension("calls"))?;
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("pr merge"))
            .count(),
        1
    );
    assert!(calls.contains("run view"));
    assert!(
        db.rows(
            "SELECT authorization FROM delivery_merge_intents WHERE task=?",
            &[&task]
        )?
        .len()
            == 1
    );
    unsafe { std::env::remove_var(key_name) };
    Ok(())
}

#[allow(dead_code)]
#[path = "support/decisions.rs"]
mod support;
