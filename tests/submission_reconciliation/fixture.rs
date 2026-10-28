use horde::{config::Settings, protocol, store::Store, template};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

pub const METHOD: &str = "submission_close_unadmitted";
pub const ORIGINAL_ACTOR: &str = "00000000-0000-4000-8000-000000000001";
pub const REPAIR_ACTOR: &str = "00000000-0000-4000-8000-000000000002";

pub struct Fixture {
    pub root: tempfile::TempDir,
    pub db: Store,
    pub repo: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary submission fixture");
        let db =
            Store::open_with_config_dir(&root.path().join("data"), &root.path().join("config"))
                .expect("isolated native Store");
        install_fixture_skills(&db, "default");
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).expect("temporary repository directory");
        horde::git::run(&repo, &["init", "-b", "main"]).expect("temporary git init");
        horde::git::run(
            &repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        )
        .expect("temporary initial commit");
        Self { root, db, repo }
    }

    pub fn second_store(&self) -> Store {
        Store::open_with_config_dir(
            &self.root.path().join("data"),
            &self.root.path().join("config"),
        )
        .expect("second connection to the same native Store")
    }

    pub fn request(&self, project: &str) -> Value {
        let operation = "op-fixture-build";
        let request_id = format!(
            "foundry:{}",
            horde::store::hash(format!("{ORIGINAL_ACTOR}:{operation}").as_bytes())
        );
        json!({
            "schema_version":1, "project":project, "request_id":request_id,
            "system_binding":{
                "organization_id":"00000000-0000-4000-8000-000000000003",
                "project_id":"00000000-0000-4000-8000-000000000004",
                "tenant_id":horde::projects::tenant(&self.db, project).expect("native project tenant"), "thread_id":"00000000-0000-4000-8000-000000000005",
                "original_actor_id":ORIGINAL_ACTOR, "operation_id":operation, "action":"build",
                "request_hash":horde::store::hash(b"original System reservation"),
                "objective_sha256":horde::store::hash(b"frozen fixture objective"),
                "invocation_id":"00000000-0000-4000-8000-000000000006"
            },
            "repair_actor_id":REPAIR_ACTOR,
            "continuity_evidence_sha256":horde::store::hash(b"source and configured-volume audit reference")
        })
    }

    pub fn close(&self, request: Value, project: &str) -> Value {
        let result = protocol::dispatch_scoped(&self.db, METHOD, request, None, Some(project));
        let counts = counts(&self.db);
        eprintln!(
            "submission reconciliation native counts tasks={} steps={} attempts={} workers={}",
            counts["tasks"], counts["steps"], counts["attempts"], counts["workers"]
        );
        result.expect("supported submission_close_unadmitted API must return the owning receipt")
    }

    pub fn other_project(&self) -> String {
        let project = protocol::dispatch(&self.db, "project_create", json!({"name":"other"}), None)
            .expect("isolated second project")["id"]
            .as_str()
            .expect("project identity")
            .to_owned();
        install_fixture_skills(&self.db, &project);
        project
    }

    pub fn stable_submit(&self, request: &Value) -> Value {
        json!({"project":request["project"], "request_id":request["request_id"],
            "repo":self.repo, "objective":"frozen fixture objective", "template":"simulated"})
    }
}

fn install_fixture_skills(db: &Store, project: &str) {
    let packet = horde::skill_catalog::load_from(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills"),
    )
    .expect("actual repository skill pack");
    assert!(
        !packet.is_empty(),
        "fixture must retain the real skill prerequisite"
    );
    let root = horde::projects::storage_root(db, project).expect("owning project skill root");
    std::fs::create_dir_all(&root).expect("isolated project storage directory");
    let installed =
        horde::skill_catalog::install(&root, &packet).expect("native skill pack installation");
    let checked = horde::skill_catalog::check(&root).expect("native installed skill pack readback");
    assert_eq!(
        checked["hash"], installed["hash"],
        "installed fixture skills are the selected catalog"
    );
}

pub fn binding_hash(request: &Value) -> String {
    horde::store::hash(
        &serde_json::to_vec(&request["system_binding"]).expect("sorted JSON binding"),
    )
}

pub fn assert_negative(receipt: &Value, request: &Value) {
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["state"], "not_admitted");
    assert_eq!(receipt["admission_fenced"], true);
    assert!(
        receipt.get("task_id").is_some_and(Value::is_null),
        "negative receipt has explicit null task"
    );
    assert_eq!(receipt["project"], request["project"]);
    assert_eq!(receipt["request_id"], request["request_id"]);
    assert_eq!(receipt["system_binding_hash"], binding_hash(request));
    assert_eq!(receipt["repair_actor_id"], request["repair_actor_id"]);
    assert_eq!(
        receipt["continuity_evidence_sha256"],
        request["continuity_evidence_sha256"]
    );
    uuid::Uuid::parse_str(receipt["receipt_id"].as_str().expect("native receipt ID"))
        .expect("UUID receipt identity");
    assert!(
        receipt["created_at"]
            .as_i64()
            .is_some_and(|value| value > 0)
    );
}

pub fn counts(db: &Store) -> Value {
    let mut values = serde_json::Map::new();
    for table in [
        "tasks",
        "steps",
        "attempts",
        "workers",
        "project_submission_receipts",
        "run_bindings",
        "task_projects",
        "revisions",
        "events",
        "task_skills",
        "artifact_links",
        "task_tree",
        "context_records",
        "external_ops",
    ] {
        let count: i64 = db
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table}{}",
                    if table == "events" {
                        " WHERE task IS NOT NULL"
                    } else {
                        ""
                    }
                ),
                [],
                |row| row.get(0),
            )
            .expect("native fixture table count");
        values.insert(table.to_owned(), json!(count));
    }
    Value::Object(values)
}

pub fn fence_count(db: &Store, project: &str, request_id: &str) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM project_submission_fences WHERE project=? AND request_id=?",
            [project, request_id],
            |row| row.get(0),
        )
        .expect("native fence journal count")
}

/// The exact former outer-atomic task/context/receipt ordering, without the new API guard.
pub fn old_atomic_submit(db: &Store, request: &Value) -> anyhow::Result<Value> {
    let repo = PathBuf::from(request["repo"].as_str().expect("fixture repository"));
    let project = request["project"].as_str().expect("fixture project");
    let plan = template::compile(
        "simulated",
        &template::load_templates(&repo.join("absent"))?,
        BTreeMap::from([("task".into(), "frozen fixture objective".into())]),
    )?;
    db.atomic(|| {
        let task = db.submit_project(
            project,
            "frozen fixture objective",
            &repo,
            &Settings {
                autonomy: true,
                ..Settings::default()
            },
            &plan,
        )?;
        horde::run::bind_run_context(
            db,
            &task,
            Some("00000000-0000-4000-8000-000000000005"),
            Some("00000000-0000-4000-8000-000000000005"),
        )?;
        let response = json!({"id":task});
        db.conn.execute(
            "INSERT INTO project_submission_receipts VALUES(?,?,?,?,?)",
            rusqlite::params![
                project,
                request["request_id"].as_str().expect("stable request"),
                horde::store::hash(&serde_json::to_vec(request)?),
                task,
                response.to_string()
            ],
        )?;
        Ok(response)
    })
}
