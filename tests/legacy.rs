use horde::store::Store;
use rusqlite::Connection;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let c = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    c.execute_batch(include_str!("fixtures/legacy-outcome-schema.sql"))
        .unwrap();
    c.execute_batch("INSERT INTO outcomes VALUES('root','Preserve this objective','/repo','running','{}','{}',1);
INSERT INTO tasks VALUES('step','root','work','{}','running',NULL);
INSERT INTO workers VALUES('worker','root','step','running','hash','/workspace','branch','base',1);
INSERT INTO attempts VALUES('attempt','step','worker','running',1,NULL,123,NULL,NULL);
INSERT INTO claims VALUES('root','src','worker');
INSERT INTO messages VALUES(1,'message','root','worker','outcome','original body','{}',1,1);
INSERT INTO outcome_tree VALUES('root','root',NULL,NULL,0,NULL,NULL,1,'{}');
INSERT INTO outcome_bundles VALUES('root','app','hash');
").unwrap();
    dir
}

#[test]
fn legacy_upgrade_preserves_identity_ownership_and_original_records() {
    let dir = fixture();
    // Reproduce tables left behind by the failing 0.4/0.5 updater.
    let c = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    c.execute_batch(
        "CREATE TABLE steps(id TEXT PRIMARY KEY, task TEXT REFERENCES tasks(id));
CREATE TABLE task_tree(task TEXT PRIMARY KEY REFERENCES tasks(id));
CREATE TABLE task_bundles(task TEXT);",
    )
    .unwrap();
    drop(c);
    let db = Store::open(dir.path()).unwrap();
    let task = db.task("root").unwrap();
    assert_eq!(task["objective"], "Preserve this objective");
    assert_eq!(task["status"], "blocked");
    assert_eq!(
        db.conn
            .query_row("SELECT step FROM workers WHERE id='worker'", [], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "step"
    );
    assert_eq!(
        db.conn
            .query_row("SELECT worker FROM claims WHERE task='root'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "worker"
    );
    assert_eq!(
        db.conn
            .query_row("SELECT body FROM messages", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "original body"
    );
    assert!(
        !db.conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    drop(db);
    Store::open(dir.path()).unwrap();
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pre-horde-rename-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = Connection::open(&backups[0]).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT status FROM outcomes", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "running"
    );
}

#[test]
fn legacy_upgrade_refuses_to_replace_populated_new_tables() {
    let dir = fixture();
    let c = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    c.execute_batch("CREATE TABLE steps(id TEXT); INSERT INTO steps VALUES('other');")
        .unwrap();
    assert!(
        Store::open(dir.path())
            .err()
            .unwrap()
            .to_string()
            .contains("populated steps")
    );
    assert_eq!(
        c.query_row("SELECT objective FROM outcomes", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "Preserve this objective"
    );
}

#[test]
fn legacy_upgrade_rolls_back_when_references_are_broken() {
    let dir = fixture();
    let c = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    c.execute("UPDATE claims SET worker='missing'", []).unwrap();
    assert!(
        Store::open(dir.path())
            .err()
            .unwrap()
            .to_string()
            .contains("broken references")
    );
    assert_eq!(
        c.query_row("SELECT outcome FROM tasks", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "root"
    );
}

#[test]
fn legacy_upgrade_refuses_live_daemons_and_remote_work() {
    use fs2::FileExt;
    let dir = fixture();
    let lock = std::fs::File::create(dir.path().join("daemon.lock")).unwrap();
    lock.lock_exclusive().unwrap();
    assert!(
        Store::open(dir.path())
            .err()
            .unwrap()
            .to_string()
            .contains("stop the legacy")
    );
    drop(lock);
    let c = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    c.execute(
        "INSERT INTO remote_origins VALUES('root','peer','remote-root')",
        [],
    )
    .unwrap();
    assert!(
        Store::open(dir.path())
            .err()
            .unwrap()
            .to_string()
            .contains("federation records")
    );
    assert_eq!(
        c.query_row("SELECT outcome FROM tasks", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "root"
    );
}
