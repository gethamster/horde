use horde::codex_session::Session;
use serde_json::json;

#[tokio::test]
async fn incompatible_app_server_fails_before_thread_start() {
    let mut command = horde::executor::clean_command("python3");
    command.args(["-u", "-c", r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' in r:
  print(json.dumps({'id':r['id'],'error':{'code':-32601,'message':'secret-do-not-echo'}}),flush=True)
"#]);
    let mut session = Session::spawn(command, None).unwrap();
    let error = session.initialize().await.unwrap_err().to_string();
    assert!(error.contains("incompatible"));
    assert!(!error.contains("secret-do-not-echo"));
}

#[tokio::test]
async fn app_server_request_protocol_and_external_auth() {
    let mut command = horde::executor::clean_command("python3");
    command.args([
        "-u",
        "-c",
        r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 if r['method']=='initialize':
  assert r['params']['capabilities']['experimentalApi']
 if r['method']=='account/login/start':
  assert r['params']['type']=='chatgptAuthTokens'
  assert 'refreshToken' not in r['params']
 print(json.dumps({'id':r['id'],'result':{}}),flush=True)
"#,
    ]);
    let mut session = Session::spawn(command, None).unwrap();
    session.initialize().await.unwrap();
    session
        .request(
            "account/login/start",
            json!({"type":"chatgptAuthTokens","accessToken":"test","chatgptAccountId":"a"}),
        )
        .await
        .unwrap();
}

fn managed_account(db: &horde::store::Store, provider: &str, mode: &str) -> String {
    horde::accounts::dispatch(db,"account_create",&json!({"project":"default","name":"test","provider":provider,"auth_mode":mode,"base_url":""})).unwrap().unwrap()["id"].as_str().unwrap().to_owned()
}

#[test]
fn managed_claude_profile_has_explicit_auth_and_no_ambient_ssh() {
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(temp.path()).unwrap();
    let account = managed_account(&db, "claude", "login");
    horde::accounts::set_credential(
        &db,
        "default",
        &account,
        &horde::accounts::Credential {
            kind: "claude_setup_token".into(),
            secret: "selected-setup-token".into(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
    let config = horde::config::ExecutorConfig {
        kind: "claude".into(),
        auth_mode: "login".into(),
        account: Some(account.clone()),
        base_url: "".into(),
        ..Default::default()
    };
    let command = horde::account_auth::command(&db, "default", &account, &config).unwrap();
    let env: std::collections::BTreeMap<_, _> = command
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert_eq!(
        env["CLAUDE_CODE_OAUTH_TOKEN"].as_deref(),
        Some("selected-setup-token")
    );
    assert!(
        env["HOME"]
            .as_deref()
            .unwrap()
            .starts_with(temp.path().to_str().unwrap())
    );
    assert!(
        env["CLAUDE_CONFIG_DIR"]
            .as_deref()
            .unwrap()
            .contains(&account)
    );
    assert!(env.get("SSH_AUTH_SOCK").is_none_or(Option::is_none));
}

#[tokio::test]
async fn concurrent_codex_sessions_share_single_controller_refresh_owner() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(temp.path()).unwrap();
    let account = managed_account(&db, "codex", "login");
    let program = temp.path().join("mock-codex");
    std::fs::write(
        &program,
        r#"#!/usr/bin/env python3
import json,sys,os,time
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 if r['method']=='account/read':
  assert r['params']['refreshToken'] is True
  path=os.path.join(os.environ['CODEX_HOME'],'auth.json')
  auth=json.load(open(path))
  assert auth['tokens']['refresh_token']=='controller-refresh'
  time.sleep(.1)
  auth['tokens']['access_token']='worker-access'
  json.dump(auth,open(path,'w'))
  open(os.path.join(os.environ['CODEX_HOME'],'refresh-count'),'a').write('refresh\n')
 print(json.dumps({'id':r['id'],'result':{}}),flush=True)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    horde::accounts::set_credential(&db,"default",&account,&horde::accounts::Credential {kind:"codex_refresh_token".into(),secret:json!({"tokens":{"refresh_token":"controller-refresh","access_token":"old-access","id_token":"id","account_id":"workspace"}}).to_string(),expires_at:None,metadata:json!({})}).unwrap();
    let program = program.to_str().unwrap();
    let (a, b) = tokio::join!(
        horde::account_auth::access_tokens(&db, "default", &account, false, Some(program)),
        horde::account_auth::access_tokens(&db, "default", &account, false, Some(program))
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(
        a.access_token == "worker-access",
        "first worker token mismatch"
    );
    assert!(
        b.access_token == "worker-access",
        "second worker token mismatch"
    );
    assert!(
        !serde_json::to_string(&a)
            .unwrap()
            .contains("controller-refresh")
    );
    let count = std::fs::read_to_string(
        temp.path()
            .join("private/account-refresh")
            .join(&account)
            .join("refresh-count"),
    )
    .unwrap();
    assert_eq!(count.lines().count(), 1);
    assert!(
        !horde::accounts::profile_directory(temp.path(), "default", &account)
            .unwrap()
            .join("codex/auth.json")
            .exists()
    );
}

#[tokio::test]
async fn managed_codex_turn_refreshes_without_persisting_auth_messages() {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(&temp.path().join("data")).unwrap();
    let account = managed_account(&db, "codex", "login");
    horde::accounts::set_credential(&db,"default",&account,&horde::accounts::Credential {kind:"codex_refresh_token".into(),secret:json!({"tokens":{"refresh_token":"controller-refresh","access_token":"initial-token","id_token":"id","account_id":"workspace"}}).to_string(),expires_at:None,metadata:json!({})}).unwrap();
    let program = temp.path().join("mock-codex");
    std::fs::write(&program,r#"#!/usr/bin/env python3
import json,sys,os
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 if r.get('id')=='refresh':
  assert 'refreshToken' not in r['result']
  token=r['result']['accessToken']
  print(json.dumps({'method':'item/completed','params':{'item':{'type':'agentMessage','text':json.dumps({'accepted':True,'result':'done '+token})}}}),flush=True)
  print(json.dumps({'method':'thread/tokenUsage/updated','params':{'tokenUsage':{'total':{'inputTokens':12,'outputTokens':4}}}}),flush=True)
  print(json.dumps({'method':'turn/completed','params':{'turn':{'status':'completed'}}}),flush=True)
  continue
 result={}
 if r['method']=='account/read':
  path=os.path.join(os.environ['CODEX_HOME'],'auth.json')
  auth=json.load(open(path));auth['tokens']['access_token']='refreshed-access-token'
  json.dump(auth,open(path,'w'))
 if r['method']=='account/login/start':
  assert r['params']['type']=='chatgptAuthTokens'
  assert not os.path.exists(os.path.join(os.environ['CODEX_HOME'],'auth.json'))
 if r['method']=='thread/start':
  assert r['params']['sandbox']=='workspace-write'
  assert r['params']['approvalPolicy']=='never'
  assert 'mcp_servers.coordination.command' in r['params']['config']
  result={'thread':{'id':'thread'}}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
 if r['method']=='turn/start':
  print(json.dumps({'id':'refresh','method':'account/chatgptAuthTokens/refresh','params':{}}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let settings = horde::config::Settings {
        allow_commands: true,
        ..Default::default()
    };
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(temp.path()).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    let task = db.submit("test", temp.path(), &settings, &plan).unwrap();
    let row = db.steps(&task).unwrap()[0].clone();
    let step = horde::store::Store::step(&row).unwrap();
    let worker = db.register(&task, row["id"].as_str()).unwrap();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES('test',?,?,'running',?)",
            rusqlite::params![
                row["id"].as_str().unwrap(),
                worker["id"].as_str().unwrap(),
                horde::store::now()
            ],
        )
        .unwrap();
    db.conn.execute("INSERT INTO attempt_bindings SELECT 'test','default','local',account,id,credential_version,'native' FROM auth_profiles WHERE account=?",[&account]).unwrap();
    let invocation = horde::executor::Invocation {
        db: &db,
        task: &task,
        step: row["id"].as_str().unwrap(),
        attempt: "test",
        worker: worker["id"].as_str().unwrap(),
        token: worker["token"].as_str().unwrap(),
        workspace: temp.path(),
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    let config = horde::config::ExecutorConfig {
        kind: "codex".into(),
        auth_mode: "login".into(),
        base_url: "".into(),
        account: Some(account.clone()),
        program: Some(program.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let result = horde::codex_session::execute(&invocation, &config, "default", &account)
        .await
        .unwrap();
    assert_eq!(result["accepted"], true);
    assert!(result["result"].as_str().unwrap().contains("[REDACTED]"));
    assert!(!result.to_string().contains("refreshed-access-token"));
    assert_eq!(result["usage"]["provider"]["input_tokens"], 12);
}

#[tokio::test]
async fn cancelling_app_server_stops_its_descendant_process_group() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("should-not-exist");
    let mut command = horde::executor::clean_command("python3");
    command
        .args([
            "-u",
            "-c",
            r#"import os,time,json,sys
pid=os.fork()
if pid==0:
 time.sleep(.3)
 open(sys.argv[1],'w').write('escaped')
 os._exit(0)
print(json.dumps({'pid':pid}),flush=True)
time.sleep(20)
"#,
        ])
        .arg(&marker);
    let mut session = Session::spawn(command, None).unwrap();
    let spawned = session.receive().await.unwrap();
    assert!(spawned["pid"].as_u64().unwrap() > 1);
    drop(session);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !marker.exists(),
        "a child survived cancellation and performed work"
    );
}

#[tokio::test]
async fn native_project_commands_keep_home_and_caches_separate() {
    use std::collections::BTreeMap;
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(&temp.path().join("data")).unwrap();
    let mut homes = Vec::new();
    for slug in ["horde", "hamster"] {
        let project = horde::projects::dispatch(&db, "project_create", &json!({"slug":slug}))
            .unwrap()
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let repo = temp.path().join(slug);
        std::fs::create_dir(&repo).unwrap();
        horde::git::run(&repo, &["init", "-b", "main"]).unwrap();
        let plan = horde::template::compile(
            "simulated",
            &horde::template::load_templates(&repo).unwrap(),
            BTreeMap::from([("task".into(), "test".into())]),
        )
        .unwrap();
        let task = db
            .submit_project(&project, "test", &repo, &Default::default(), &plan)
            .unwrap();
        let argv=vec!["python3".into(),"-c".into(),"import os,json; print(json.dumps({k:os.getenv(k) for k in ['HOME','XDG_CACHE_HOME','SSH_AUTH_SOCK']}))".into()];
        let result = horde::executor::run_task_command_env(
            &db,
            &task,
            &argv,
            &repo,
            5,
            None,
            &BTreeMap::from([("HOME".into(), "/unapproved-home".into())]),
        )
        .await
        .unwrap();
        assert_eq!(result["success"], true);
        let environment: serde_json::Value =
            serde_json::from_str(result["stdout"].as_str().unwrap()).unwrap();
        assert!(environment["HOME"].as_str().unwrap().contains(&project));
        assert!(
            environment["XDG_CACHE_HOME"]
                .as_str()
                .unwrap()
                .contains(&project)
        );
        assert!(environment["SSH_AUTH_SOCK"].is_null());
        homes.push(environment["HOME"].clone());
    }
    assert_ne!(homes[0], homes[1]);
}

#[tokio::test]
async fn rotating_claude_credentials_does_not_leak_the_previous_token() {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(&temp.path().join("data")).unwrap();
    let account = managed_account(&db, "claude", "login");
    let credential = |secret: &str| horde::accounts::Credential {
        kind: "claude_setup_token".into(),
        secret: secret.into(),
        expires_at: None,
        metadata: json!({}),
    };
    horde::accounts::set_credential(&db, "default", &account, &credential("old-private-token"))
        .unwrap();
    let program = temp.path().join("mock-claude");
    let started = temp.path().join("started");
    let rotated = temp.path().join("rotated");
    std::fs::write(&program,format!(r#"#!/usr/bin/env python3
import os,json,time
open({started:?},'w').close()
while not os.path.exists({rotated:?}): time.sleep(.01)
print(json.dumps({{'result':json.dumps({{'accepted':True,'result':os.environ['CLAUDE_CODE_OAUTH_TOKEN']}}),'usage':{{'input_tokens':1}}}}))
"#,started=started.to_str().unwrap(),rotated=rotated.to_str().unwrap())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let settings = horde::config::Settings {
        allow_commands: true,
        providers: BTreeMap::from([(
            "claude".into(),
            horde::config::Provider {
                kind: "claude".into(),
                auth_mode: "login".into(),
                base_url: "".into(),
                account: Some(account.clone()),
                program: Some(program.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )]),
        executors: BTreeMap::from([(
            "worker".into(),
            horde::config::Executor {
                provider: Some("claude".into()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(temp.path()).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    let task = db.submit("test", temp.path(), &settings, &plan).unwrap();
    let row = db.steps(&task).unwrap()[0].clone();
    let mut step = horde::store::Store::step(&row).unwrap();
    step.kind = "agent".into();
    step.role = "worker".into();
    let worker = db.register(&task, row["id"].as_str()).unwrap();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES('test',?,?,'running',?)",
            rusqlite::params![
                row["id"].as_str().unwrap(),
                worker["id"].as_str().unwrap(),
                horde::store::now()
            ],
        )
        .unwrap();
    db.conn.execute("INSERT INTO attempt_bindings SELECT 'test','default','local',account,id,credential_version,'native' FROM auth_profiles WHERE account=?",[&account]).unwrap();
    let invocation = horde::executor::Invocation {
        db: &db,
        task: &task,
        step: row["id"].as_str().unwrap(),
        attempt: "test",
        worker: worker["id"].as_str().unwrap(),
        token: worker["token"].as_str().unwrap(),
        workspace: temp.path(),
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    let rotate = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        horde::accounts::set_credential(&db, "default", &account, &credential("new-private-token"))
            .unwrap();
        std::fs::write(&rotated, b"ready").unwrap();
    };
    let (result, ()) = tokio::join!(horde::executor::execute(&invocation), rotate);
    let result = result.unwrap();
    assert_eq!(result["result"], "[REDACTED]");
    let hash = result["events_artifact"].as_str().unwrap();
    let artifact = std::fs::read_to_string(db.root.join("artifacts").join(hash)).unwrap();
    assert!(!artifact.contains("old-private-token"));
    assert!(!artifact.contains("new-private-token"));
}

#[tokio::test]
async fn refresh_child_retains_lock_when_controller_descriptor_closes() {
    use fs2::FileExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("refresh.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    lock.lock_exclusive().unwrap();
    let mut command = horde::executor::clean_command("python3");
    command.args([
        "-u",
        "-c",
        "import json,os,time; print(json.dumps({'ready':True,'pid':os.getpid()}),flush=True); time.sleep(30)",
    ]);
    let mut session = Session::spawn_locked(command, None, Some(&lock)).unwrap();
    let ready = session.receive().await.unwrap();
    assert_eq!(ready["ready"], true);
    let pid = ready["pid"].as_i64().unwrap() as i32;
    drop(lock);
    let next = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert!(
        next.try_lock_exclusive().is_err(),
        "orphan refresh must retain exclusive ownership"
    );
    session.stop().await;
    assert!(
        !horde::executor::process_alive(pid),
        "stop must return after the refresh child has exited"
    );
    // Other tests in this binary spawn processes too. A child forked while `lock` was
    // open holds a copy of it, and with it the flock, until that child execs, so wait
    // for the lock instead of expecting it to be free at once.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while let Err(error) = next.try_lock_exclusive() {
        assert!(
            error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline,
            "stopping the refresh child must release its lock: {error}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[test]
fn runtime_revoked_while_waiting_for_refresh_cannot_receive_access_token() {
    use fs2::FileExt;
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(temp.path()).unwrap();
    let account = managed_account(&db, "codex", "login");
    horde::accounts::set_credential(
        &db,
        "default",
        &account,
        &horde::accounts::Credential {
            kind: "codex_access_token".into(),
            secret: "worker-access".into(),
            expires_at: None,
            metadata: json!({"account_id":"workspace"}),
        },
    )
    .unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks VALUES('owner','work','repo','running','{}','{}',0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES('owner','default',NULL)",
            [],
        )
        .unwrap();
    db.conn.execute("INSERT INTO remote_links(task,peer,remote_id,state,request,base) VALUES('owner','worker','remote','running','assignment',NULL)",[]).unwrap();
    db.conn.execute("INSERT INTO account_remote_reservations SELECT 'lease','default',account,'worker','owner','attempt','worker',id,credential_version,'active',0 FROM auth_profiles WHERE account=?",[&account]).unwrap();
    let directory = db.root.join("private/account-refresh").join(&account);
    std::fs::create_dir_all(&directory).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("refresh.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let root = db.root.clone();
    let (started, ready) = std::sync::mpsc::channel();
    let call = std::thread::spawn(move || {
        let db = horde::store::Store::open(&root).unwrap();
        started.send(()).unwrap();
        horde::account_auth::remote_tokens(
            &db,
            "worker",
            &json!({"project":"default","account":account,"task":"owner","remote_task":"remote"}),
        )
    });
    ready.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        !call.is_finished(),
        "refresh must wait for the existing owner"
    );
    horde::projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":"default","runtime":"worker"}),
    )
    .unwrap();
    drop(lock);
    let error = call.join().unwrap().unwrap_err();
    assert!(
        error.to_string().contains("runtime grant revoked"),
        "expected a revoked runtime grant rejection"
    );
}
