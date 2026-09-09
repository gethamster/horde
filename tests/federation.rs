use horde::{
    network::{DirectPeer, NetworkConfig, Provider},
    store::Store,
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Processes(Vec<Child>);
impl Drop for Processes {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
fn config(
    dir: &Path,
    name: &str,
    ca: &rcgen::Certificate,
    issuer: &Issuer<'_, KeyPair>,
) -> (NetworkConfig, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![format!("{name}.test")]).unwrap();
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params.signed_by(&key, issuer).unwrap();
    let pem = dir.join(format!("{name}.pem"));
    let keyfile = dir.join(format!("{name}.key"));
    let cafile = dir.join(format!("{name}.ca.pem"));
    horde::secrets::write_private(&keyfile, key.serialize_pem().as_bytes()).unwrap();
    std::fs::write(&pem, cert.pem()).unwrap();
    std::fs::write(&cafile, ca.pem()).unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    (
        NetworkConfig {
            provider: Provider::Direct,
            runtime_id: name.into(),
            port,
            timeout_seconds: 5,
            ca_cert: cafile,
            identity_cert: pem,
            identity_key: keyfile,
            ..Default::default()
        },
        hex::encode(Sha256::digest(cert.der())),
    )
}
fn cli(root: &Path, user: &Path, args: &[&str], token: Option<&str>) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_horde"));
    command
        .arg("--data-dir")
        .arg(root)
        .args(args)
        .env("XDG_CONFIG_HOME", user);
    if let Some(token) = token {
        command.env("HORDE_WORKER_TOKEN", token);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn wait(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for runtime state"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn spawn(processes: &mut Processes, root: &Path, user: &Path, args: &[&str]) {
    processes.0.push(
        Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(root)
            .args(args)
            .env("XDG_CONFIG_HOME", user)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
}
#[test]
fn independent_runtimes_preserve_context_route_questions_share_secrets_and_verify_results() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let root = dir.join("root");
    let remote = dir.join("remote");
    Store::open(&root).unwrap();
    Store::open(&remote).unwrap();
    let rootuser = dir.join("root-user");
    let remoteuser = dir.join("remote-user");
    std::fs::create_dir_all(rootuser.join("horde")).unwrap();
    std::fs::create_dir_all(remoteuser.join("horde")).unwrap();
    let source = dir.join("app.env");
    horde::secrets::write_private(&source, b"APP_SECRET=fixture-secret-never-in-context\n")
        .unwrap();
    std::fs::write(
        rootuser.join("horde/secrets.toml"),
        format!("[bundles]\napp = {:?}\n", source),
    )
    .unwrap();
    let skill_source = dir.join("root-only-skill");
    std::fs::create_dir_all(skill_source.join("references")).unwrap();
    std::fs::write(
        skill_source.join("SKILL.md"),
        "Use the distributed report skill. Read references/format.md.",
    )
    .unwrap();
    std::fs::write(
        skill_source.join("references/format.md"),
        "Report CSV without email addresses.",
    )
    .unwrap();
    std::fs::write(
        rootuser.join("horde/config.toml"),
        format!(
            "secret_bundles = ['app']\nknowledge_topics = ['bench']\n[skills]\nreport = {:?}\n",
            skill_source
        ),
    )
    .unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&key).unwrap();
    let issuer = Issuer::new(params, key);
    let (mut a, af) = config(dir, "root", &ca, &issuer);
    let (mut b, bf) = config(dir, "worker", &ca, &issuer);
    a.allowed_clients.insert(bf, "worker".into());
    b.allowed_clients.insert(af, "root".into());
    a.peers.insert(
        "worker".into(),
        DirectPeer {
            address: format!("127.0.0.1:{}", b.port).parse().unwrap(),
            tls_name: "worker.test".into(),
        },
    );
    b.peers.insert(
        "root".into(),
        DirectPeer {
            address: format!("127.0.0.1:{}", a.port).parse().unwrap(),
            tls_name: "root.test".into(),
        },
    );
    a.delegate_peers = vec!["worker".into()];
    b.execution_clients = vec!["root".into()];
    a.share_bundles.insert("worker".into(), vec!["app".into()]);
    b.receive_bundles.insert("root".into(), vec!["app".into()]);
    let afile = dir.join("root-network.toml");
    let bfile = dir.join("worker-network.toml");
    std::fs::write(&afile, toml::to_string(&a).unwrap()).unwrap();
    std::fs::write(&bfile, toml::to_string(&b).unwrap()).unwrap();
    let mut processes = Processes(vec![]);
    spawn(
        &mut processes,
        &root,
        &rootuser,
        &["network", "--config", afile.to_str().unwrap(), "listen"],
    );
    spawn(
        &mut processes,
        &remote,
        &remoteuser,
        &["network", "--config", bfile.to_str().unwrap(), "listen"],
    );
    wait(|| {
        root.join("network-runtime.toml").exists() && remote.join("network-runtime.toml").exists()
    });
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".horde/templates")).unwrap();
    std::fs::write(repo.join("server.py"),"import os,http.server,socketserver\nprint(os.environ['APP_SECRET'],flush=True)\nsocketserver.TCPServer(('127.0.0.1',int(os.environ['PORT'])),http.server.SimpleHTTPRequestHandler).serve_forever()\n").unwrap();
    std::fs::write(repo.join("test.py"),"import os,urllib.request,pathlib\nassert os.environ['APP_SECRET']\nassert urllib.request.urlopen(os.environ['HORDE_APP_URL']).status==200\npathlib.Path('result.txt').write_text('verified')\n").unwrap();
    std::fs::write(repo.join(".horde/templates/app.toml"),"name='app'\nversion='1.0.0'\ninputs=['task']\n[[steps]]\nid='test'\nkind='environment'\n[steps.environment]\nstart=['python3','server.py']\ntest=['sh','-c','python3 test.py && git add result.txt && git commit -m verified']\ntimeout_seconds=20\nreadiness_seconds=5\n").unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@localhost"],
        vec!["add", "."],
        vec!["commit", "-m", "fixture"],
    ] {
        horde::git::run(&repo, &args).unwrap();
    }
    spawn(&mut processes, &root, &rootuser, &["daemon"]);
    wait(|| root.join("daemon.sock").exists());
    let oid = cli(
        &root,
        &rootuser,
        &[
            "submit",
            "Implement without exporting identifying fields",
            "--repo",
            repo.to_str().unwrap(),
            "--template",
            "simulated",
        ],
        None,
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    cli(&root,&rootuser,&["call","update_context",&json!({"task":oid,"content":"Never export email addresses","provenance":"original caller statement 7"}).to_string()],None);
    std::fs::remove_dir_all(&skill_source).unwrap();
    let request=json!({"task":oid,"id":"remote-once","objective":"Run the app and verify it","template":"app","peer":"worker","skills":["report"]}).to_string();
    let child = cli(&root, &rootuser, &["call", "delegate_task", &request], None)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        cli(&root, &rootuser, &["call", "delegate_task", &request], None)["id"],
        child
    );
    let rootdb = Store::open(&root).unwrap();
    wait(|| {
        rootdb
            .rows(
                "SELECT remote_id FROM remote_links WHERE task=? AND remote_id IS NOT NULL",
                &[&child],
            )
            .unwrap()
            .len()
            == 1
    });
    let remoteid = rootdb
        .rows("SELECT remote_id FROM remote_links WHERE task=?", &[&child])
        .unwrap()[0]["remote_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let remotedb = Store::open(&remote).unwrap();
    assert_eq!(
        horde::skills::catalog(&rootdb, &child).unwrap(),
        horde::skills::catalog(&remotedb, &remoteid).unwrap()
    );
    assert!(
        horde::skills::read(&remotedb, &remoteid, &json!({"name":"report"})).unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("distributed report skill")
    );

    let input = rootdb
        .rows(
            "SELECT hash FROM artifact_links WHERE task=? AND name='federation-input'",
            &[&child],
        )
        .unwrap();
    let mut packet: Value = serde_json::from_slice(
        &std::fs::read(
            root.join("artifacts")
                .join(input[0]["hash"].as_str().unwrap()),
        )
        .unwrap(),
    )
    .unwrap();
    packet["bundles"] = json!({});
    assert_eq!(
        horde::federation::call_sync(a.clone(), "worker".into(), "accept".into(), packet.clone())
            .unwrap()["duplicate"],
        true
    );
    packet["objective"] = json!("different assignment");
    assert!(
        horde::federation::call_sync(a.clone(), "worker".into(), "accept".into(), packet).is_err()
    );
    assert!(
        horde::federation::call_sync(b.clone(), "root".into(), "accept".into(), json!({})).is_err()
    );
    assert!(
        horde::federation::call_sync(
            a.clone(),
            "worker".into(),
            "status".into(),
            json!({"task":"someone-else"})
        )
        .is_err()
    );
    let step = remotedb.steps(&remoteid).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let worker = remotedb.register(&remoteid, Some(&step)).unwrap();
    // Notebook operations use the root authority over the existing mTLS route.
    let sibling = horde::delegation::delegate(
        &rootdb,
        &oid,
        &json!({"id":"notebook-sibling","objective":"supporting work","template":"simulated"}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let root_claim=horde::protocol::dispatch(&rootdb,"add_knowledge",json!({"task":oid,"scope":"family","topic":"bench","kind":"evidence","content":"root shared measurement","provenance":{}}),None).unwrap();
    horde::protocol::dispatch(&rootdb,"add_knowledge",json!({"task":sibling,"scope":"family","kind":"fact","content":"sibling shared measurement","provenance":{}}),None).unwrap();
    horde::protocol::dispatch(&rootdb,"add_knowledge",json!({"task":sibling,"kind":"fact","content":"sibling private measurement","provenance":{}}),None).unwrap();
    let shared = horde::protocol::dispatch(
        &remotedb,
        "knowledge",
        json!({"scope":"family","query":"measurement"}),
        worker["token"].as_str(),
    )
    .unwrap();
    assert_eq!(shared["records"].as_array().unwrap().len(), 2, "{shared}");
    assert!(!shared.to_string().contains("private"));
    let options = horde::protocol::dispatch(
        &remotedb,
        "knowledge_options",
        json!({}),
        worker["token"].as_str(),
    )
    .unwrap();
    assert_eq!(
        options["schemas"]["add_knowledge"]["properties"]["topic"]["enum"],
        json!(["bench"])
    );
    let remote_claim=horde::protocol::dispatch(&remotedb,"add_knowledge",json!({"id":"remote-measurement","scope":"family","kind":"evidence","content":"remote shared measurement","provenance":{"run":"test"},"verified":true}),worker["token"].as_str()).unwrap();
    let shared = horde::protocol::dispatch(
        &rootdb,
        "knowledge",
        json!({"task":sibling,"scope":"family","query":"remote"}),
        None,
    )
    .unwrap();
    let record = &shared["records"][0];
    assert_eq!(record["task"], child);
    assert_eq!(record["origin"]["step"], step);
    assert_eq!(record["origin"]["remote_task"], remoteid);
    assert_eq!(record["verified"], 0);
    assert!(
        horde::protocol::dispatch(
            &remotedb,
            "retract_knowledge",
            json!({"id":root_claim["id"],"reason":"not mine","provenance":{}}),
            worker["token"].as_str()
        )
        .is_err()
    );
    horde::protocol::dispatch(
        &remotedb,
        "retract_knowledge",
        json!({"id":remote_claim["id"],"reason":"measurement withdrawn","provenance":{}}),
        worker["token"].as_str(),
    )
    .unwrap();
    assert_eq!(
        horde::protocol::dispatch(
            &rootdb,
            "knowledge",
            json!({"task":sibling,"scope":"family","query":"remote"}),
            None
        )
        .unwrap()["records"],
        json!([])
    );

    let resource = horde::protocol::dispatch(
        &remotedb,
        "read_skill",
        json!({"name":"report","path":"references/format.md"}),
        worker["token"].as_str(),
    )
    .unwrap();
    assert_eq!(resource["content"], "Report CSV without email addresses.");
    assert!(
        Path::new(resource["base_directory"].as_str().unwrap())
            .starts_with(remote.canonicalize().unwrap())
    );

    remotedb
        .conn
        .execute("UPDATE tasks SET status='paused' WHERE id=?", [&remoteid])
        .unwrap();
    spawn(&mut processes, &remote, &remoteuser, &["daemon"]);
    wait(|| remote.join("daemon.sock").exists());
    let q=cli(&remote,&remoteuser,&["call","request_question",&json!({"id":"format","question":"Should the test artifact use CSV?","evidence":"original caller statement 7"}).to_string()],worker["token"].as_str());
    let mut mcp = Command::new(env!("CARGO_BIN_EXE_horde"))
        .arg("--data-dir")
        .arg(&root)
        .arg("mcp")
        .env("XDG_CONFIG_HOME", &rootuser)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    writeln!(mcp.stdin.take().unwrap(),"{}",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"answer_question","arguments":{"task":oid,"question":q["id"],"answer":"Yes, use CSV and exclude email addresses"}}})).unwrap();
    let result = mcp.wait_with_output().unwrap();
    assert!(result.status.success());
    assert!(!String::from_utf8_lossy(&result.stdout).contains("\"isError\":true"));
    wait(|| {
        remotedb
            .rows(
                "SELECT answer FROM questions WHERE id=?",
                &[&q["id"].as_str()],
            )
            .unwrap()
            .first()
            .is_some_and(|q| !q["answer"].is_null())
    });
    // A remote caller delegates from its own committed workspace. The root still
    // owns the tree and the remote parent must verify the grandchild result.
    let parent_workspace =
        horde::git::allocate(&remotedb, &remoteid, worker["id"].as_str().unwrap()).unwrap();
    std::fs::write(parent_workspace.join("parent.txt"), "parent context").unwrap();
    horde::git::run(&parent_workspace, &["add", "parent.txt"]).unwrap();
    horde::git::run(&parent_workspace, &["commit", "-m", "parent change"]).unwrap();
    let nested = cli(
        &remote,
        &remoteuser,
        &[
            "call",
            "delegate_task",
            &json!({"id":"nested-once","objective":"Check parent context","template":"simulated"})
                .to_string(),
        ],
        worker["token"].as_str(),
    );
    let grandchild = nested["id"].as_str().unwrap();
    assert_eq!(
        horde::delegation::tree(&rootdb, grandchild).unwrap()["parent"],
        child
    );
    assert_eq!(
        horde::delegation::tree(&rootdb, grandchild).unwrap()["depth"],
        2
    );
    wait(|| rootdb.task(grandchild).unwrap()["status"] == "succeeded");
    let nested_workspace = horde::git::task_workspace(&rootdb, grandchild).unwrap();
    assert_eq!(
        std::fs::read_to_string(nested_workspace.join("parent.txt")).unwrap(),
        "parent context"
    );
    cli(&remote,&remoteuser,&["call","integrate_child",&json!({"child":grandchild,"validation":["python3","-c","from pathlib import Path; assert Path('parent.txt').read_text()=='parent context'"]}).to_string()],worker["token"].as_str());
    assert!(horde::delegation::child_completion(&rootdb, &child).unwrap());
    wait(|| {
        remotedb
            .messages(worker["id"].as_str().unwrap(), 0, 100)
            .unwrap()
            .as_array()
            .is_some_and(|m| !m.is_empty())
    });
    let mail = remotedb
        .messages(worker["id"].as_str().unwrap(), 0, 100)
        .unwrap();
    let ids: Vec<_> = mail
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    cli(
        &remote,
        &remoteuser,
        &[
            "call",
            "acknowledge_messages",
            &json!({"ids":ids}).to_string(),
        ],
        worker["token"].as_str(),
    );
    cli(&remote, &remoteuser, &["resume", &remoteid], None);
    wait(|| rootdb.task(&child).unwrap()["status"] == "succeeded");
    let contract = horde::delegation::mandatory(&remotedb, &remoteid)
        .unwrap()
        .to_string();
    assert!(contract.contains("Never export email addresses"));
    assert!(contract.contains("Yes, use CSV"));
    assert!(!contract.contains("fixture-secret-never-in-context"));
    assert_eq!(
        remotedb
            .rows(
                "SELECT state FROM app_environments WHERE task=?",
                &[&remoteid]
            )
            .unwrap()[0]["state"],
        "removed"
    );
    cli(&root,&rootuser,&["call","integrate_child",&json!({"task":oid,"child":child,"validation":["python3","-c","from pathlib import Path; assert Path('result.txt').read_text()=='verified'"]}).to_string()],None);
    let before: i64 = rootdb
        .conn
        .query_row(
            "SELECT MAX(revision) FROM revisions WHERE task=?",
            [&child],
            |r| r.get(0),
        )
        .unwrap();
    let revision =
        json!({"task":child,"steps":[{"id":"reverify","kind":"command","command":["true"]}]})
            .to_string();
    cli(&root, &rootuser, &["call", "add_steps", &revision], None);
    cli(&root, &rootuser, &["call", "add_steps", &revision], None);
    let after: i64 = rootdb
        .conn
        .query_row(
            "SELECT MAX(revision) FROM revisions WHERE task=?",
            [&child],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        after,
        before + 1,
        "remote revision retries must advance notification generation exactly once"
    );
    assert!(
        rootdb
            .rows("SELECT * FROM child_acceptance WHERE child=?", &[&child])
            .unwrap()
            .is_empty()
    );
    wait(|| rootdb.task(&child).unwrap()["status"] == "succeeded");
    cli(
        &root,
        &rootuser,
        &[
            "call",
            "integrate_child",
            &json!({"task":oid,"child":child,"validation":["true"]}).to_string(),
        ],
        None,
    );
    wait(|| !remote.join("remote-secrets").join(&remoteid).exists());
    let workspace = horde::git::task_workspace(&rootdb, &oid).unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("result.txt")).unwrap(),
        "verified"
    );
    assert!(!repo.join("result.txt").exists());
    for db in [&rootdb, &remotedb] {
        assert!(
            !db.rows("SELECT data FROM events", &[])
                .unwrap()
                .iter()
                .any(|v| v.to_string().contains("fixture-secret-never-in-context"))
        );
    }
}
#[test]
fn snapshot_integrity_and_escaping_entries_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        horde::federation::unpack(&json!({"hash":"wrong","archive":"00"}), temp.path()).is_err()
    );
}

#[test]
fn remote_answer_sync_does_not_reopen_consumed_questions() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let plan = horde::template::Plan {
        steps: vec![],
        pins: Default::default(),
        outputs: Default::default(),
    };
    let oid = db
        .submit("question receipts", dir.path(), &Default::default(), &plan)
        .unwrap();
    let worker = db.register(&oid, None).unwrap();
    db.conn
        .execute(
            "INSERT INTO questions VALUES('q',?,'Which format?',NULL)",
            [&oid],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO question_context VALUES('q','input',?)",
            [worker["id"].as_str()],
        )
        .unwrap();
    let answer = json!([{"id":"q","answer":"CSV"}]);
    horde::federation::apply_remote_answers(&db, &oid, &answer).unwrap();
    assert_eq!(
        db.rows("SELECT purpose FROM question_context", &[])
            .unwrap()[0]["purpose"],
        "input_answered"
    );
    db.conn
        .execute(
            "UPDATE question_context SET purpose='input_consumed' WHERE question='q'",
            [],
        )
        .unwrap();
    horde::federation::apply_remote_answers(&db, &oid, &answer).unwrap();
    assert_eq!(
        db.rows("SELECT purpose FROM question_context", &[])
            .unwrap()[0]["purpose"],
        "input_consumed"
    );
}

#[test]
fn repository_writing_skill_survives_remote_snapshot() {
    fn copy_tree(source: &Path, target: &Path) {
        let metadata = std::fs::symlink_metadata(source).unwrap();
        if metadata.file_type().is_symlink() {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(std::fs::read_link(source).unwrap(), target).unwrap();
        } else if metadata.is_dir() {
            std::fs::create_dir_all(target).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                copy_tree(&entry.path(), &target.join(entry.file_name()));
            }
        } else {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(source, target).unwrap();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in [
        ".agents/skills/writer-responsible-prose",
        ".claude/skills/writer-responsible-prose",
    ] {
        copy_tree(&source.join(path), &repo.join(path));
    }
    for argv in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "."],
        vec!["commit", "-m", "Add writing skill"],
    ] {
        horde::git::run(&repo, &argv).unwrap();
    }
    let snapshot = horde::federation::snapshot(&repo).unwrap();
    let remote = temp.path().join("remote");
    horde::federation::unpack(&snapshot, &remote).unwrap();
    for path in [
        ".agents/skills/writer-responsible-prose/SKILL.md",
        ".agents/skills/writer-responsible-prose/references/guardrails.md",
        ".claude/skills/writer-responsible-prose/SKILL.md",
    ] {
        assert_eq!(
            std::fs::read(source.join(path)).unwrap(),
            std::fs::read(remote.join(path)).unwrap()
        );
    }
    let entrypoint = remote.join(".claude/skills/writer-responsible-prose/SKILL.md");
    assert!(
        entrypoint
            .parent()
            .unwrap()
            .join("../../../.agents/skills/writer-responsible-prose/SKILL.md")
            .canonicalize()
            .unwrap()
            .starts_with(remote.canonicalize().unwrap())
    );
}
