use horde::{config::Settings, delegation, protocol, store::Store, template};
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    root: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            horde::git::run(&repo, &args).unwrap();
        }
        let db = Store::open(&dir.path().join("data")).unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(&repo).unwrap(),
            BTreeMap::from([("task".into(), "test".into())]),
        )
        .unwrap();
        let settings = Settings {
            knowledge_topics: vec!["performance".into(), "correctness".into()],
            ..Default::default()
        };
        let root = db.submit("test", &repo, &settings, &plan).unwrap();
        Self {
            _dir: dir,
            db,
            root,
        }
    }
    fn child(&self, parent: &str, id: &str) -> String {
        delegation::delegate(
            &self.db,
            parent,
            &json!({"id":id,"objective":"work","template":"simulated"}),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn call(&self, task: &str, name: &str, mut args: Value) -> anyhow::Result<Value> {
        args["task"] = json!(task);
        protocol::dispatch(&self.db, name, args, None)
    }
    fn add(&self, task: &str, scope: &str, content: &str) -> String {
        self.call(task,"add_knowledge",json!({"scope":scope,"kind":"evidence","content":content,"provenance":{"test":"fixture"},"topic":"performance","valid_under":{"commit":"abc","flags":{"fast":true}}})).unwrap()["id"].as_str().unwrap().into()
    }
}

#[test]
fn children_share_only_published_family_claims_and_never_change_execution_state() {
    let f = Fixture::new();
    let a = f.child(&f.root, "a");
    let b = f.child(&f.root, "b");
    let grandchild = f.child(&a, "grandchild");
    let shared = f.add(&f.root, "family", "root observation");
    let sibling = f.add(&a, "family", "sibling observation");
    let private = f.add(&a, "task", "private observation");
    let before = delegation::tree(&f.db, &f.root).unwrap()["version"].clone();
    let states =
        f.db.rows("SELECT id,status FROM tasks ORDER BY id", &[])
            .unwrap();
    for task in [&b, &grandchild] {
        let page = f
            .call(task, "knowledge", json!({"scope":"family"}))
            .unwrap();
        let rows = page["records"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{page}");
        assert!(rows.iter().any(|r| r["id"] == shared));
        assert!(rows.iter().any(|r| r["id"] == sibling));
        assert!(!page.to_string().contains(&private));
        assert_eq!(rows[0]["valid_under"]["commit"], "abc");
        assert_eq!(rows[0]["origin"]["task"], f.root);
        assert!(
            !delegation::contract(&f.db, task, 0, 100)
                .unwrap()
                .to_string()
                .contains("observation")
        );
        assert!(
            !delegation::mandatory(&f.db, task)
                .unwrap()
                .to_string()
                .contains("observation")
        );
    }
    let original = f.db.task(&f.root).unwrap();
    let repo = std::path::Path::new(original["repo"].as_str().unwrap());
    let plan = template::compile(
        "simulated",
        &template::load_templates(repo).unwrap(),
        BTreeMap::from([("task".into(), "independent".into())]),
    )
    .unwrap();
    let separate =
        f.db.submit("independent", repo, &Settings::default(), &plan)
            .unwrap();
    assert_eq!(
        f.call(&separate, "knowledge", json!({"scope":"family"}))
            .unwrap()["records"],
        json!([])
    );
    assert_eq!(delegation::tree(&f.db, &f.root).unwrap()["version"], before);
    for state in states {
        assert_eq!(
            f.db.task(state["id"].as_str().unwrap()).unwrap()["status"],
            state["status"]
        );
    }
    let old = f.call(&a, "knowledge", json!({})).unwrap();
    assert!(old.is_array());
    assert_eq!(old.as_array().unwrap().len(), 2);
    assert!(old[0].get("scope").is_none());
}

#[test]
fn ranked_search_topic_filters_and_cursors_are_bounded_and_detect_changes() {
    let f = Fixture::new();
    let best = f.add(&f.root, "family", "latency latency latency");
    f.add(
        &f.root,
        "family",
        "latency with many unrelated details about implementation and deployment",
    );
    f.add(&f.root, "task", "latency private");
    f.call(&f.root,"add_knowledge",json!({"scope":"family","kind":"fact","content":"latency","topic":"correctness","provenance":{}})).unwrap();
    let first = f
        .call(
            &f.root,
            "knowledge",
            json!({"scope":"family","query":"latency","topic":"performance","limit":1}),
        )
        .unwrap();
    assert_eq!(first["records"][0]["id"], best);
    let second=f.call(&f.root,"knowledge",json!({"scope":"family","query":"latency","topic":"performance","limit":1,"after":first["next"]})).unwrap();
    assert_eq!(second["records"].as_array().unwrap().len(), 1);
    assert!(second["next"].is_null());
    assert_ne!(second["records"][0]["id"], first["records"][0]["id"]);
    assert!(
        f.call(
            &f.root,
            "knowledge",
            json!({"scope":"task","query":"latency","after":first["next"]})
        )
        .is_err()
    );
    f.add(&f.root, "family", "another latency observation");
    let error = f
        .call(
            &f.root,
            "knowledge",
            json!({"scope":"family","query":"latency","topic":"performance","after":first["next"]}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("restart"));
    assert!(
        f.call(&f.root, "knowledge", json!({"scope":"family","query":"\""}))
            .is_err()
    );
    let reopened = Store::open(&f.db.root).unwrap();
    let found = protocol::dispatch(
        &reopened,
        "knowledge",
        json!({"task":f.root,"scope":"family","query":"observation"}),
        None,
    )
    .unwrap();
    assert_eq!(found["records"].as_array().unwrap().len(), 1);
}

#[test]
fn lifecycle_preserves_provenance_and_enforces_writer_authority() {
    let f = Fixture::new();
    let a = f.child(&f.root, "a");
    let b = f.child(&f.root, "b");
    let old = f.add(&a, "family", "old result");
    let wa = f.db.register(&a, None).unwrap();
    let wb = f.db.register(&b, None).unwrap();
    let token = wa["token"].as_str().unwrap();
    let version = delegation::tree(&f.db, &a).unwrap()["version"].clone();
    let fresh=protocol::dispatch(&f.db,"add_knowledge",json!({"id":"revision-1","scope":"family","kind":"evidence","content":"new result","provenance":{"run":"two"},"supersedes":[old],"verified":true}),Some(token)).unwrap();
    let visible = f.call(&b, "knowledge", json!({"scope":"family"})).unwrap();
    assert_eq!(visible["records"].as_array().unwrap().len(), 1);
    assert_eq!(visible["records"][0]["verified"], 0);
    let all = f
        .call(
            &b,
            "knowledge",
            json!({"scope":"family","include_inactive":true}),
        )
        .unwrap();
    let original = all["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == old)
        .unwrap();
    assert_eq!(original["superseded"], true);
    assert_eq!(original["superseded_by"], fresh["id"]);
    assert_eq!(original["content"], "old result");
    assert!(
        protocol::dispatch(
            &f.db,
            "retract_knowledge",
            json!({"id":fresh["id"],"reason":"disagree","provenance":{}}),
            Some(wb["token"].as_str().unwrap())
        )
        .is_err()
    );
    protocol::dispatch(
        &f.db,
        "retract_knowledge",
        json!({"id":fresh["id"],"reason":"invalid measurement","provenance":{"run":"two"}}),
        Some(token),
    )
    .unwrap();
    assert_eq!(
        f.call(&b, "knowledge", json!({"scope":"family"})).unwrap()["records"],
        json!([])
    );
    let all = f
        .call(
            &b,
            "knowledge",
            json!({"scope":"family","include_inactive":true}),
        )
        .unwrap();
    assert!(
        all["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["retracted"] == true && r["retraction"]["reason"] == "invalid measurement")
    );
    assert_eq!(delegation::tree(&f.db, &a).unwrap()["version"], version);
    let private = f.add(&a, "task", "private");
    assert!(
        f.call(
            &b,
            "link_knowledge",
            json!({"source":fresh["id"],"target":private,"relation":"contradicts"})
        )
        .is_err()
    );
}

#[test]
fn topics_and_scopes_are_discoverable_and_ids_are_idempotent() {
    let f = Fixture::new();
    let w = f.db.register(&f.root, None).unwrap();
    let options = protocol::dispatch(
        &f.db,
        "knowledge_options",
        json!({}),
        Some(w["token"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!(
        options["schemas"]["add_knowledge"]["properties"]["topic"]["enum"],
        json!(["performance", "correctness"])
    );
    for name in ["knowledge", "add_knowledge"] {
        for schema in [protocol::schema(name), protocol::admin_schema(name)] {
            assert_eq!(
                schema["properties"]["scope"]["enum"],
                json!(["task", "family"])
            );
        }
    }
    assert!(
        f.call(
            &f.root,
            "add_knowledge",
            json!({"kind":"fact","scope":"global","content":"x","provenance":{}})
        )
        .is_err()
    );
    assert!(
        f.call(
            &f.root,
            "add_knowledge",
            json!({"kind":"fact","topic":"unknown","content":"x","provenance":{}})
        )
        .is_err()
    );
    let args = json!({"id":"stable","kind":"fact","content":"hello","provenance":{}});
    f.call(&f.root, "add_knowledge", args.clone()).unwrap();
    assert_eq!(
        f.call(&f.root, "add_knowledge", args).unwrap()["duplicate"],
        true
    );
    assert!(
        f.call(
            &f.root,
            "add_knowledge",
            json!({"id":"stable","kind":"fact","content":"different","provenance":{}})
        )
        .is_err()
    );
}

#[test]
fn pages_are_byte_bounded_and_legacy_supporting_context_is_private() {
    let f = Fixture::new();
    let child = f.child(&f.root, "child");
    let mut expected = std::collections::BTreeSet::new();
    for _ in 0..6 {
        expected.insert(f.add(&f.root, "family", &"x".repeat(60000)));
    }
    let mut after = Value::Null;
    let mut found = std::collections::BTreeSet::new();
    loop {
        let mut args = json!({"scope":"family","limit":100});
        if after.is_string() {
            args["after"] = after;
        }
        let page = f.call(&child, "knowledge", args).unwrap();
        assert!(serde_json::to_vec(&page["records"]).unwrap().len() < 256 * 1024);
        for row in page["records"].as_array().unwrap() {
            assert!(found.insert(row["id"].as_str().unwrap().to_owned()));
        }
        after = page["next"].clone();
        if after.is_null() {
            break;
        }
    }
    assert_eq!(found, expected);
    let private = f.add(&f.root, "task", "old private supporting claim");
    f.db.conn
        .execute(
            "INSERT INTO context_records VALUES(?,?,1,'supporting_fact',?,'legacy',0)",
            rusqlite::params![private, f.root, "old private supporting claim"],
        )
        .unwrap();
    assert!(
        !delegation::contract(&f.db, &child, 0, 100)
            .unwrap()
            .to_string()
            .contains("old private")
    );
    assert!(
        !delegation::mandatory(&f.db, &child)
            .unwrap()
            .to_string()
            .contains(&private)
    );
    assert!(
        delegation::contract(&f.db, &f.root, 0, 100)
            .unwrap()
            .to_string()
            .contains("old private")
    );
    f.db.conn
        .execute(
            "INSERT INTO remote_origins VALUES(?,'owner-peer','owner-child')",
            [&child],
        )
        .unwrap();
    let cached = json!({"context":{"supporting_sources":[{"id":"root-private","kind":"supporting_fact","provenance":json!({"source_task":"owner-root"}).to_string()},{"id":"own-private","kind":"supporting_fact","provenance":json!({"source_task":"owner-child"}).to_string()}]}});
    f.db.conn
        .execute(
            "INSERT INTO remote_context VALUES(?,?)",
            rusqlite::params![child, cached.to_string()],
        )
        .unwrap();
    let filtered = delegation::mandatory(&f.db, &child).unwrap();
    assert_eq!(filtered["supporting_sources"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["supporting_sources"][0]["id"], "own-private");
}

#[test]
fn migration_indexes_old_rows_without_publishing_them() {
    let dir = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("state.sqlite3")).unwrap();
    connection.execute_batch("CREATE TABLE tasks(id TEXT PRIMARY KEY,objective TEXT NOT NULL,repo TEXT NOT NULL,status TEXT NOT NULL,settings TEXT NOT NULL,plan TEXT NOT NULL,created INTEGER NOT NULL); CREATE TABLE knowledge(id TEXT PRIMARY KEY,task TEXT NOT NULL,step TEXT,kind TEXT NOT NULL,content TEXT NOT NULL,provenance TEXT NOT NULL,verified INTEGER NOT NULL,inputs TEXT NOT NULL); PRAGMA user_version=3;").unwrap();
    connection
        .execute(
            "INSERT INTO tasks VALUES('old-task','old objective','.','succeeded',?,'{}',0)",
            [serde_json::to_string(&Settings::default()).unwrap()],
        )
        .unwrap();
    connection.execute_batch("INSERT INTO knowledge VALUES('old-claim','old-task',NULL,'evidence','legacy searchable evidence','{}',1,'{}')").unwrap();
    drop(connection);
    let db = Store::open(dir.path()).unwrap();
    let result = protocol::dispatch(
        &db,
        "knowledge",
        json!({"task":"old-task","scope":"task","query":"searchable"}),
        None,
    )
    .unwrap();
    assert_eq!(result["records"][0]["id"], "old-claim");
    assert_eq!(result["records"][0]["scope"], "task");
    assert_eq!(result["records"][0]["verified"], 1);
    assert_eq!(
        protocol::dispatch(
            &db,
            "knowledge",
            json!({"task":"old-task","scope":"family"}),
            None
        )
        .unwrap()["records"],
        json!([])
    );
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    assert_eq!(
        protocol::dispatch(
            &db,
            "knowledge",
            json!({"task":"old-task","scope":"task","query":"searchable"}),
            None
        )
        .unwrap()["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn schema_four_upgrade_preserves_execution_receipts_and_skill_revisions() {
    // The relevant v4 layout predates notebook metadata and its FTS index.
    let dir = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("state.sqlite3")).unwrap();
    connection.execute_batch("CREATE TABLE tasks(id TEXT PRIMARY KEY,objective TEXT NOT NULL,repo TEXT NOT NULL,status TEXT NOT NULL,settings TEXT NOT NULL,plan TEXT NOT NULL,created INTEGER NOT NULL); CREATE TABLE knowledge(id TEXT PRIMARY KEY,task TEXT NOT NULL,step TEXT,kind TEXT NOT NULL,content TEXT NOT NULL,provenance TEXT NOT NULL,verified INTEGER NOT NULL,inputs TEXT NOT NULL);").unwrap();
    connection.execute("INSERT INTO tasks VALUES('v4-task','keep execution constraints','.','succeeded',?,'{}',0)", [serde_json::to_string(&Settings::default()).unwrap()]).unwrap();
    connection.execute_batch("INSERT INTO knowledge VALUES('v4-claim','v4-task',NULL,'evidence','searchable v4 evidence','{}',1,'{}')").unwrap();
    horde::skills::migrate(&connection).unwrap();
    horde::execution_selection::migrate(&connection).unwrap();
    horde::submission::migrate(&connection).unwrap();
    let old = Store {
        conn: connection,
        root: dir.path().to_owned(),
    };
    let binding = json!({"runtime":"local","capability":"fixture","provider":"fixture","kind":"simulated","model":null,"configuration_hash":"a".repeat(64)});
    let policy = json!({"version":1,"allowed":[{"runtime":"local","capabilities":["fixture"]}],"bindings":[binding.clone()],"selected":binding});
    horde::execution_selection::pin(&old, "v4-task", &policy).unwrap();
    old.conn
        .execute(
            "INSERT INTO submission_receipts VALUES('submission-v4','pinned-request','v4-task',?)",
            [json!({"id":"v4-task"}).to_string()],
        )
        .unwrap();
    let skill_dir = dir.path().join("skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "Preserve this accepted skill revision.",
    )
    .unwrap();
    let packet =
        horde::skills::capture(dir.path(), &BTreeMap::from([("review".into(), skill_dir)]))
            .unwrap();
    let bundle = serde_json::to_string(&packet["review"]).unwrap();
    old.conn.execute("INSERT INTO skill_policy_proposals VALUES('proposal-v4','project','review','base',0,'baseline',?,0,'reviewed','accepted',0,1)", [&bundle]).unwrap();
    old.conn.execute("INSERT INTO skill_policy_revisions VALUES('project','review',1,?,0,'proposal-v4','reviewed',0)", [&bundle]).unwrap();
    old.conn
        .execute(
            "INSERT INTO skill_policy_heads VALUES('project','review',1,?)",
            [&bundle],
        )
        .unwrap();
    let tables = [
        "task_execution_policy",
        "submission_receipts",
        "skill_policy_proposals",
        "skill_policy_revisions",
        "skill_policy_heads",
    ];
    let before: Vec<_> = tables
        .iter()
        .map(|table| old.rows(&format!("SELECT * FROM {table}"), &[]).unwrap())
        .collect();
    old.conn.pragma_update(None, "user_version", 4).unwrap();
    assert!(
        old.rows(
            "SELECT name FROM sqlite_master WHERE name='knowledge_meta'",
            &[]
        )
        .unwrap()
        .is_empty()
    );
    drop(old);

    for _ in 0..2 {
        let db = Store::open(dir.path()).unwrap();
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            horde::execution_selection::policy(&db, "v4-task").unwrap(),
            Some(policy.clone())
        );
        for (table, expected) in tables.iter().zip(&before) {
            assert_eq!(
                &db.rows(&format!("SELECT * FROM {table}"), &[]).unwrap(),
                expected,
                "{table}"
            );
        }
        let page = protocol::dispatch(
            &db,
            "knowledge",
            json!({"task":"v4-task","scope":"task","query":"searchable"}),
            None,
        )
        .unwrap();
        assert_eq!(page["records"].as_array().unwrap().len(), 1);
        assert_eq!(page["records"][0]["id"], "v4-claim");
        assert_eq!(page["records"][0]["scope"], "task");
        assert_eq!(
            protocol::dispatch(
                &db,
                "knowledge",
                json!({"task":"v4-task","scope":"family"}),
                None
            )
            .unwrap()["records"],
            json!([])
        );
    }
}

#[test]
fn relationship_pages_export_all_visible_edges_without_private_targets() {
    let f = Fixture::new();
    let child = f.child(&f.root, "reader");
    let source = f.add(&f.root, "family", "source");
    let target = f.add(&f.root, "family", "target");
    let private = f.add(&f.root, "task", "private target");
    for n in 0..101 {
        f.call(
            &f.root,
            "link_knowledge",
            json!({"source":source,"target":target,"relation":format!("edge-{n:03}")}),
        )
        .unwrap();
    }
    f.call(
        &f.root,
        "link_knowledge",
        json!({"source":source,"target":private,"relation":"hidden"}),
    )
    .unwrap();
    let page = f
        .call(
            &child,
            "knowledge",
            json!({"scope":"family","query":"source"}),
        )
        .unwrap();
    let record = &page["records"][0];
    assert_eq!(record["edges"].as_array().unwrap().len(), 100);
    assert_eq!(record["edges_truncated"], true);
    let rest = f
        .call(
            &child,
            "knowledge_edges",
            json!({"source":source,"after":record["edges_next"]}),
        )
        .unwrap();
    assert_eq!(rest["edges"].as_array().unwrap().len(), 1);
    assert_eq!(rest["edges"][0]["relation"], "edge-100");
    assert!(rest["next"].is_null());
    assert!(!page.to_string().contains(&private));
    assert!(!rest.to_string().contains(&private));
}
