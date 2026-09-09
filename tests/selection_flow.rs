//! The parent chooses its split; fake provider processes exercise real scheduling and TLS.
use horde::{
    network::{DirectPeer, NetworkConfig, Provider},
    store::Store,
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon {
    process: Child,
    root: PathBuf,
}

impl Daemon {
    fn start(root: &Path, user: &Path) -> Self {
        let process = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(root)
            .arg("daemon")
            .env("XDG_CONFIG_HOME", user)
            .env_remove("HORDE_ENROLLMENT_FILE")
            .env_remove("HORDE_ENROLLMENT_JSON")
            .env_remove("HORDE_WORKER_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let daemon = Self {
            process,
            root: root.to_owned(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.join("daemon.sock").exists() {
            assert!(Instant::now() < deadline, "daemon startup timed out");
            std::thread::sleep(Duration::from_millis(30));
        }
        daemon
    }

    fn call(&self, method: &str, args: Value) -> Value {
        let mut socket =
            std::os::unix::net::UnixStream::connect(self.root.join("daemon.sock")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(socket, "{}", json!({"method":method,"args":args})).unwrap();
        let mut line = String::new();
        std::io::BufReader::new(socket)
            .read_line(&mut line)
            .unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        reply["result"].clone()
    }

    fn wait_update(&self, request: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let inspected = self.call("runtime_inspect", json!({"id":"apollo"}));
            let operation = inspected["operations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|op| op["id"] == request)
                .unwrap();
            if operation["state"] == "succeeded" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "update did not complete: {operation}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_success(&self, task: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let status = self.call("inspect", json!({"task":task}));
            if status["task"]["status"] == "succeeded" {
                return status;
            }
            assert!(Instant::now() < deadline, "task did not complete: {status}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn identity(
    dir: &Path,
    name: &str,
    ca: &str,
    issuer: &Issuer<'_, KeyPair>,
) -> (NetworkConfig, String) {
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec![format!("{name}.test")])
        .unwrap()
        .signed_by(&key, issuer)
        .unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = NetworkConfig {
        provider: Provider::Direct,
        runtime_id: name.into(),
        port: socket.local_addr().unwrap().port(),
        timeout_seconds: 2,
        ca_cert: dir.join(format!("{name}.ca")),
        identity_cert: dir.join(format!("{name}.cert")),
        identity_key: dir.join(format!("{name}.key")),
        ..Default::default()
    };
    std::fs::write(&config.ca_cert, ca).unwrap();
    std::fs::write(&config.identity_cert, cert.pem()).unwrap();
    horde::secrets::write_private(&config.identity_key, key.serialize_pem().as_bytes()).unwrap();
    (config, horde::store::hash(cert.der()))
}

fn fake_harness(dir: &Path, name: &str, codex: bool) -> PathBuf {
    let program = dir.join(format!("fake-{name}"));
    let log = dir.join(format!("{name}-models"));
    let output = if codex {
        r#"printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"{\"accepted\":true,\"result\":\"parent completed\"}"}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'"#
    } else {
        r#"printf '%s\n' '{"result":"{\"accepted\":true,\"result\":\"child completed\"}","usage":{}}'"#
    };
    let script = format!(
        r#"#!/bin/sh
set -eu
model=
while [ $# -gt 0 ]; do
  if [ "$1" = --model ]; then shift; model=$1; fi
  shift
done
cat > '{prompt}'
printf '%s\n' "$model" >> '{log}'
printf '%s\n' "$model" > {name}-model.txt
git -c core.hooksPath=/dev/null add {name}-model.txt
git -c core.hooksPath=/dev/null -c user.name=Test -c user.email=test@localhost commit -q -m '{name} result'
{output}
"#,
        log = log.display(),
        prompt = dir.join(format!("{name}-prompt")).display()
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

fn user_config(dir: &Path, name: &str, config: &str) -> PathBuf {
    let user = dir.join(format!("{name}-config"));
    std::fs::create_dir_all(user.join("horde")).unwrap();
    std::fs::write(user.join("horde/config.toml"), config).unwrap();
    user
}

#[test]
fn parent_selects_local_astra_and_delegates_one_narrowed_apollo_glm_child() {
    // Short paths keep both real daemon Unix sockets within macOS's 104-byte limit.
    let dir = tempfile::Builder::new()
        .prefix("horde-selection-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@localhost"],
    ] {
        horde::git::run(&repo, &args).unwrap();
    }
    let templates = repo.join(".horde/templates");
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::write(templates.join("selected.toml"), "name = 'selected'\nversion = '1'\ninputs = ['task']\n[[steps]]\nid = 'work'\nkind = 'agent'\nrole = 'worker'\ninstructions = 'Perform the supplied task'\nscope = ['.']\n").unwrap();
    std::fs::write(repo.join("original.txt"), "caller-owned content\n").unwrap();
    horde::git::run(&repo, &["add", "."]).unwrap();
    horde::git::run(&repo, &["commit", "-m", "fixture"]).unwrap();
    let original_head = horde::git::run(&repo, &["rev-parse", "HEAD"]).unwrap();
    let controller_db = Store::open(&dir.path().join("controller")).unwrap();
    let worker_db = Store::open(&dir.path().join("apollo")).unwrap();
    let profile = horde::fleet::Profile {
        provider: "tailscale".into(),
        peer: Some("apollo".into()),
        ..Default::default()
    };
    controller_db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('apollo','test',?,'ready',0)", [serde_json::to_string(&profile).unwrap()]).unwrap();

    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::from_ca_cert_pem(&ca.pem(), ca_key).unwrap();
    let (mut controller_net, controller_fingerprint) =
        identity(dir.path(), "controller", &ca.pem(), &issuer);
    let (mut worker_net, worker_fingerprint) = identity(dir.path(), "apollo", &ca.pem(), &issuer);
    controller_net
        .allowed_clients
        .insert(worker_fingerprint, "apollo".into());
    controller_net.delegate_peers.push("apollo".into());
    worker_net
        .allowed_clients
        .insert(controller_fingerprint, "controller".into());
    worker_net.execution_clients.push("controller".into());
    worker_net.management_clients.push("controller".into());
    worker_net.controller_peer = Some("controller".into());
    controller_net.peers.insert(
        "apollo".into(),
        DirectPeer {
            address: format!("127.0.0.1:{}", worker_net.port).parse().unwrap(),
            tls_name: "apollo.test".into(),
        },
    );
    worker_net.peers.insert(
        "controller".into(),
        DirectPeer {
            address: format!("127.0.0.1:{}", controller_net.port)
                .parse()
                .unwrap(),
            tls_name: "controller.test".into(),
        },
    );
    for (db, config) in [(&controller_db, &controller_net), (&worker_db, &worker_net)] {
        std::fs::write(
            db.root.join("managed-network.toml"),
            toml::to_string(config).unwrap(),
        )
        .unwrap();
        horde::federation::configure(&db.root, config).unwrap();
    }
    let local_program = fake_harness(dir.path(), "parent", true);
    let remote_program = fake_harness(dir.path(), "child", false);
    let local_user = user_config(
        dir.path(),
        "controller",
        &format!(
            "autonomy = true\n[providers.codex]\nprogram = {local_program:?}\nmodel = 'astra'\n"
        ),
    );
    let remote_user = user_config(
        dir.path(),
        "apollo",
        &format!(
            "autonomy = true\n[providers.claude]\nprogram = {remote_program:?}\nmodel = 'opus'\n[executors.codex]\nprovider = 'claude'\nmodel = 'astra'\n[executors.glm]\nprovider = 'claude'\nmodel = 'glm-5.3'\n"
        ),
    );
    let worker = Daemon::start(&worker_db.root, &remote_user);
    let controller = Daemon::start(&controller_db.root, &local_user);
    let deadline = Instant::now() + Duration::from_secs(15);
    let inventory = loop {
        let inventory = controller.call("runtime_capabilities", json!({}));
        if inventory["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|runtime| {
                runtime["runtime"] == "apollo"
                    && runtime["fresh"] == true
                    && runtime["protocol"]["features"]
                        .as_array()
                        .is_some_and(|features| {
                            features
                                .iter()
                                .any(|feature| feature == "runtime_skills_update")
                        })
            })
        {
            break inventory;
        }
        assert!(
            Instant::now() < deadline,
            "worker did not advertise update support: {inventory}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(inventory["runtimes"].as_array().unwrap().len(), 2);
    let plan = controller.call(
        "plan_execution",
        json!({"roles":{
            "thinking":{"runtime":"local","models":["codex"]},
            "implementation":{"runtime":"apollo","models":["claude","codex","glm"]}
        }}),
    );
    assert_eq!(plan["ready"], true, "{plan}");
    assert_eq!(plan["dispatch_started"], false);
    assert!(
        controller_db
            .rows("SELECT id FROM tasks", &[])
            .unwrap()
            .is_empty()
    );
    assert_eq!(plan["roles"]["thinking"]["choices"][0]["model"], "astra");
    let choices = plan["roles"]["implementation"]["choices"]
        .as_array()
        .unwrap();
    assert_eq!(
        choices
            .iter()
            .map(|c| c["model"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["opus", "astra", "glm-5.3"]
    );

    let pack_dir = dir.path().join("editable-skills");
    let skill_dir = pack_dir.join("routing");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("horde.toml"), "[injection]\nagent = true\n").unwrap();
    let original_instruction =
        "ORIGINAL_ROUTING_POLICY: use bounded delivery and verify the result.";
    std::fs::write(skill_dir.join("SKILL.md"), original_instruction).unwrap();
    let original_pack = controller.call("skill_pack_install", json!({"path":pack_dir}));
    let parent = controller.call("submit_task", json!({"request_id":"parent-selection","objective":"Think locally, then assign implementation within the allowed pool","repo":repo,"template":"selected","execution":{"allowed":plan["allowed"],"selected":{"runtime":"local","capability":"codex"}}}));
    let parent_id = parent["id"].as_str().unwrap();
    let completed = controller.wait_success(parent_id);
    assert_eq!(completed["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("parent-models")).unwrap(),
        "astra\n"
    );
    assert!(
        controller_db
            .rows("SELECT task FROM task_tree WHERE parent=?", &[&parent_id])
            .unwrap()
            .is_empty()
    );
    let original_prompt = std::fs::read_to_string(dir.path().join("parent-prompt")).unwrap();
    assert_eq!(original_prompt.matches(original_instruction).count(), 1);
    let second_instruction = "SECOND_ROUTING_POLICY: keep delivery small and inspect its evidence.";
    std::fs::write(skill_dir.join("SKILL.md"), second_instruction).unwrap();
    let second_pack = controller.call("skill_pack_install", json!({"path":pack_dir}));
    assert_ne!(original_pack["hash"], second_pack["hash"]);
    let first_update = json!({"id":"apollo","request_id":"skills-second"});
    controller.call("runtime_skills_update", first_update.clone());
    controller.wait_update("skills-second");
    assert_eq!(worker.call("skill_pack_list", json!({})), second_pack);

    let latest_instruction = "LATEST_ROUTING_POLICY: prefer the smallest verified delivery.";
    std::fs::write(skill_dir.join("SKILL.md"), latest_instruction).unwrap();
    let latest_pack = controller.call("skill_pack_install", json!({"path":pack_dir}));
    controller.call(
        "runtime_skills_update",
        json!({"id":"apollo","request_id":"skills-latest"}),
    );
    controller.wait_update("skills-latest");
    assert_eq!(worker.call("skill_pack_list", json!({})), latest_pack);
    assert_eq!(
        controller.call("runtime_skills_update", first_update)["state"],
        "succeeded"
    );
    assert_eq!(worker.call("skill_pack_list", json!({})), latest_pack);

    let assignment = json!({"task":parent_id,"id":"implementation-1","objective":"Implement the bounded child change","template":"selected","execution":{"allowed":plan["roles"]["implementation"]["allowed"],"selected":{"runtime":"apollo","capability":"glm"}}});
    let child = controller.call("delegate_task", assignment.clone());
    let child_id = child["id"].as_str().unwrap();
    let duplicate = controller.call("delegate_task", assignment.clone());
    assert_eq!(duplicate["id"], child_id);
    assert_eq!(duplicate["duplicate"], true);
    controller.wait_success(child_id);
    assert_eq!(controller.call("delegate_task", assignment)["id"], child_id);
    assert_eq!(
        controller_db
            .rows("SELECT task FROM task_tree WHERE parent=?", &[&parent_id])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        worker_db
            .rows("SELECT id FROM attempts", &[])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("child-models")).unwrap(),
        "glm-5.3\n"
    );
    let child_prompt = std::fs::read_to_string(dir.path().join("child-prompt")).unwrap();
    assert_eq!(child_prompt.matches(original_instruction).count(), 1);
    assert!(!child_prompt.contains(second_instruction));
    assert!(!child_prompt.contains(latest_instruction));
    let receiver_tasks = worker_db.rows("SELECT id FROM tasks", &[]).unwrap();
    assert_eq!(receiver_tasks.len(), 1);
    let receiver_policy =
        horde::execution_selection::policy(&worker_db, receiver_tasks[0]["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
    let caller_policy = horde::execution_selection::policy(&controller_db, child_id)
        .unwrap()
        .unwrap();
    assert_eq!(receiver_policy, caller_policy);
    assert_eq!(
        serde_json::to_value(horde::skills::packet(&controller_db, parent_id).unwrap()).unwrap(),
        serde_json::to_value(
            horde::skills::packet(&worker_db, receiver_tasks[0]["id"].as_str().unwrap()).unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        receiver_policy["allowed"],
        plan["roles"]["implementation"]["allowed"]
    );
    assert_eq!(receiver_policy["selected"]["model"], "glm-5.3");
    let integrated = controller.call(
        "integrate_child",
        json!({"task":parent_id,"child":child_id,"validation":["git","diff","--check"]}),
    );
    assert!(integrated["integrated_head"].is_string(), "{integrated}");
    assert!(horde::delegation::child_completion(&controller_db, parent_id).unwrap());
    let combined = horde::git::task_workspace(&controller_db, parent_id).unwrap();
    assert_eq!(
        std::fs::read_to_string(combined.join("parent-model.txt")).unwrap(),
        "astra\n"
    );
    assert_eq!(
        std::fs::read_to_string(combined.join("child-model.txt")).unwrap(),
        "glm-5.3\n"
    );
    assert_eq!(
        horde::git::run(&repo, &["rev-parse", "HEAD"]).unwrap(),
        original_head
    );
    assert_eq!(
        horde::git::run(&repo, &["status", "--porcelain"]).unwrap(),
        ""
    );
    assert!(!repo.join("parent-model.txt").exists());
    assert!(!repo.join("child-model.txt").exists());
    let parent_attempts = controller_db
        .rows("SELECT id FROM attempts", &[])
        .unwrap()
        .len();
    let child_attempts = worker_db
        .rows("SELECT id FROM attempts", &[])
        .unwrap()
        .len();
    assert_eq!((parent_attempts, child_attempts), (1, 1));
    let fresh = controller.call("submit_task", json!({"request_id":"fresh-skill-policy","objective":"Use the updated delivery policy","repo":repo,"template":"selected","execution":{"allowed":plan["allowed"],"selected":{"runtime":"local","capability":"codex"}}}));
    controller.wait_success(fresh["id"].as_str().unwrap());
    let fresh_prompt = std::fs::read_to_string(dir.path().join("parent-prompt")).unwrap();
    assert_eq!(fresh_prompt.matches(latest_instruction).count(), 1);
    assert!(!fresh_prompt.contains(original_instruction));
    assert!(!fresh_prompt.contains(second_instruction));
    println!(
        "{}",
        json!({
            "demo":"agent-selected-loopback-workflow",
            "transport":"mutual TLS between two real daemons",
            "providers":"fake executable harnesses; no live provider calls",
            "role_pools":plan["allowed"],
            "parent":{"capability":completed["execution"]["selected"]["capability"],"model":completed["execution"]["selected"]["model"],"attempts":parent_attempts},
            "child":{"capability":receiver_policy["selected"]["capability"],"model":receiver_policy["selected"]["model"],"attempts":child_attempts,"inherited_narrowed_policy":receiver_policy == caller_policy},
            "skills":{"remote_update_completed":true,"duplicate_update_preserved_latest":true,"child_retained_parent_pin":true,"fresh_task_used_latest_pack":true,"latest_hash":latest_pack["hash"]},
            "duplicate_delegation_reused_child":duplicate["id"] == child["id"],
            "validation":["git","diff","--check"],
            "integrated_head":integrated["integrated_head"],
            "review_workspace_contains_both_results":combined.join("parent-model.txt").exists() && combined.join("child-model.txt").exists(),
            "caller_repository_preserved":horde::git::run(&repo, &["rev-parse","HEAD"]).unwrap() == original_head,
        })
    );
}
