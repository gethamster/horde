use horde::{accounts, capacity, config::ExecutorConfig, store::Store};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn subscription_probe_uses_selected_profile_and_never_starts_a_turn() {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open(temp.path()).unwrap();
    let mut ids = vec![];
    for name in ["chosen", "other"] {
        let account = accounts::dispatch(
            &db,
            "account_create",
            &json!({"name":name,"provider":"codex","auth_mode":"login"}),
        )
        .unwrap()
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        accounts::set_credential(&db,"default",&account,&accounts::Credential {
            kind:"codex_refresh_token".into(),
            secret:json!({"tokens":{"refresh_token":name,"access_token":"old","id_token":"id","account_id":name}}).to_string(),
            expires_at:None,metadata:json!({}),
        }).unwrap();
        ids.push(account);
    }
    let program = temp.path().join("quota-probe");
    std::fs::write(
        &program,
        r#"#!/usr/bin/env python3
import json,os,sys,time
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 method=r['method']
 assert method in ['initialize','account/read','account/login/start','account/rateLimits/read']
 result={}
 if method=='account/read':
  path=os.path.join(os.environ['CODEX_HOME'],'auth.json')
  auth=json.load(open(path))
  assert auth['tokens']['refresh_token']=='chosen'
  auth['tokens']['access_token']='selected-access'
  json.dump(auth,open(path,'w'))
 if method=='account/login/start':
  assert r['params']['accessToken']=='selected-access'
  assert not os.path.exists(os.path.join(os.environ['CODEX_HOME'],'auth.json'))
 if method=='account/rateLimits/read':
  result={'rateLimits':{'primary':{'usedPercent':25,'resetsAt':int(time.time())+600}}}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = ExecutorConfig {
        kind: "codex".into(),
        auth_mode: "login".into(),
        base_url: String::new(),
        account: Some(ids[0].clone()),
        project: Some("default".into()),
        program: Some(program.to_string_lossy().into()),
        ..Default::default()
    };
    capacity::codex_probe(&db, &config).await.unwrap();
    let observed = db.rows("SELECT * FROM account_capacity", &[]).unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["account"], ids[0]);
    assert_eq!(observed[0]["used"], 25.0);
    assert!(
        !temp
            .path()
            .join("private/account-refresh")
            .join(&ids[1])
            .exists()
    );
}

#[tokio::test]
async fn legacy_probe_ignores_notifications_and_keeps_window_reset_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open(temp.path()).unwrap();
    let program = temp.path().join("legacy-probe");
    std::fs::write(&program,r#"#!/usr/bin/env python3
import json,sys,time
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 assert r['method'] in ['initialize','account/rateLimits/read']
 print(json.dumps({'method':'notice','params':{}}),flush=True)
 result={} if r['method']=='initialize' else {'rate_limits':{'primary':{'used_percent':100,'resets_at':int(time.time())+300},'secondary':{'used_percent':10}}}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = ExecutorConfig {
        kind: "codex".into(),
        auth_mode: "login".into(),
        program: Some(program.to_string_lossy().into()),
        ..Default::default()
    };
    capacity::codex_probe(&db, &config).await.unwrap();
    assert!(!capacity::available(&db, &capacity::account(&config)).unwrap());
    db.conn
        .execute(
            "UPDATE account_capacity SET reset=0 WHERE window='primary'",
            [],
        )
        .unwrap();
    assert!(capacity::available(&db, &capacity::account(&config)).unwrap());
}

#[test]
fn provider_headers_throttle_only_the_account_that_returned_them() {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open(temp.path()).unwrap();
    let config = ExecutorConfig {
        kind: "anthropic".into(),
        account: Some("one".into()),
        ..Default::default()
    };
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in [
        ("x-ratelimit-limit-requests", "100"),
        ("x-ratelimit-remaining-requests", "75"),
        ("anthropic-ratelimit-tokens-limit", "200"),
        ("anthropic-ratelimit-tokens-remaining", "100"),
        ("retry-after", "600"),
    ] {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    capacity::ingest_headers(&db, &config, &headers, 429).unwrap();
    assert!(!capacity::available(&db, "one").unwrap());
    assert!(capacity::available(&db, "two").unwrap());
    let windows = db
        .rows(
            "SELECT window,used FROM account_capacity ORDER BY window",
            &[],
        )
        .unwrap();
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0]["used"], 25.0);
    assert_eq!(windows[1]["used"], 50.0);
    assert_eq!(windows[2]["used"], 100.0);
}
