use horde::{
    config::{Decision, DecisionMode, Settings},
    decision::typesafe::TypeSafe,
    projects,
    store::Store,
};
use serde_json::json;
#[path = "support/decisions.rs"]
#[allow(dead_code)]
mod support;
use support::{CONFIG_LOCK, OperatorConfig, request, response_json, server};

fn project_task(db: &Store) -> (String, String) {
    let project = projects::dispatch(db, "project_create", &json!({"slug":"decision-owner"}))
        .unwrap()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = "project-decision-task".to_owned();
    db.conn
        .execute(
            "INSERT INTO tasks VALUES(?,'work','repo','running','{}','{}',0)",
            [&task],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES(?,?,NULL)",
            [&task, &project],
        )
        .unwrap();
    (project, task)
}

fn configure_project(db: &Store, project: &str, decision: &Decision) {
    let path = db.root.join("approved-project-config.toml");
    std::fs::write(
        &path,
        toml::to_string(&Settings {
            decision: decision.clone(),
            ..Settings::default()
        })
        .unwrap(),
    )
    .unwrap();
    projects::dispatch(
        db,
        "project_configure",
        &json!({"project":project,"file":path}),
    )
    .unwrap();
}

#[tokio::test]
async fn project_authorization_uses_task_owner_instead_of_default_operator() {
    let _lock = CONFIG_LOCK.lock().await;
    let _default_operator = OperatorConfig::install(&Decision::default());
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    let (project, task) = project_task(&db);
    let (base_url, mut bodies) = server(vec![("200 OK", response_json(), 0)]).await;
    let key = format!("HORDE_PROJECT_DECISION_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "project-secret") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        base_url,
        api_key_env: key.clone(),
        deadline_ms: 2_000,
        max_attempts: 1,
        ..Decision::default()
    };
    configure_project(&db, &project, &decision);

    let backend = TypeSafe::new_project(decision, root.path(), &task).unwrap();
    let (_, attempts) = backend.decide_counted(&request()).await.unwrap();
    assert_eq!(attempts, 1);
    bodies.recv().await.unwrap();
    unsafe { std::env::remove_var(key) };
}

#[tokio::test]
async fn project_authorization_rechecks_its_configuration_before_retry() {
    let _lock = CONFIG_LOCK.lock().await;
    let _default_operator = OperatorConfig::install(&Decision::default());
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    let (project, task) = project_task(&db);
    let (base_url, mut bodies) = server(vec![
        ("429 Too Many Requests", "{}", 100),
        ("200 OK", response_json(), 0),
    ])
    .await;
    let key = format!("HORDE_PROJECT_DECISION_RETRY_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "project-secret") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        base_url,
        api_key_env: key.clone(),
        deadline_ms: 2_000,
        max_attempts: 2,
        ..Decision::default()
    };
    configure_project(&db, &project, &decision);
    let backend = TypeSafe::new_project(decision.clone(), root.path(), &task).unwrap();
    let call = tokio::spawn(async move { backend.decide_counted(&request()).await });
    bodies.recv().await.unwrap();
    configure_project(
        &db,
        &project,
        &Decision {
            deadline_ms: 1_999,
            ..decision
        },
    );
    let failure = call.await.unwrap().unwrap_err();
    assert_eq!(failure.attempts, 1);
    assert!(failure.to_string().contains("no longer authorizes"));
    assert!(!matches!(
        tokio::time::timeout(std::time::Duration::from_millis(200), bodies.recv()).await,
        Ok(Some(_))
    ));
    unsafe { std::env::remove_var(key) };
}

#[tokio::test]
async fn project_authorization_stops_retries_after_task_cancellation() {
    let _lock = CONFIG_LOCK.lock().await;
    let _default_operator = OperatorConfig::install(&Decision::default());
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    let (project, task) = project_task(&db);
    let (base_url, mut bodies) = server(vec![
        ("429 Too Many Requests", "{}", 100),
        ("200 OK", response_json(), 0),
    ])
    .await;
    let key = format!("HORDE_PROJECT_DECISION_CANCEL_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "project-secret") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        base_url,
        api_key_env: key.clone(),
        deadline_ms: 2_000,
        max_attempts: 2,
        ..Decision::default()
    };
    configure_project(&db, &project, &decision);
    let backend = TypeSafe::new_project(decision, root.path(), &task).unwrap();
    let call = tokio::spawn(async move { backend.decide_counted(&request()).await });
    bodies.recv().await.unwrap();
    db.conn
        .execute("UPDATE tasks SET status='cancelled' WHERE id=?", [&task])
        .unwrap();
    let failure = call.await.unwrap().unwrap_err();
    assert_eq!(failure.attempts, 1);
    assert!(failure.to_string().contains("no longer running"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), bodies.recv())
            .await
            .is_err()
    );
    unsafe { std::env::remove_var(key) };
}

#[tokio::test]
async fn completed_tasks_authorize_reviews_only_and_cancelled_tasks_authorize_neither() {
    let _lock = CONFIG_LOCK.lock().await;
    let _default_operator = OperatorConfig::install(&Decision::default());
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    let (project, task) = project_task(&db);
    let (base_url, mut bodies) = server(vec![("200 OK", response_json(), 0)]).await;
    let key = format!("HORDE_PROJECT_COMPLETED_REVIEW_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "project-secret") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        base_url,
        api_key_env: key.clone(),
        deadline_ms: 2_000,
        max_attempts: 1,
        ..Decision::default()
    };
    configure_project(&db, &project, &decision);
    db.conn
        .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&task])
        .unwrap();

    let ordinary = TypeSafe::new_project(decision.clone(), root.path(), &task).unwrap();
    assert_eq!(
        ordinary
            .decide_counted(&request())
            .await
            .unwrap_err()
            .attempts,
        0
    );
    let review = TypeSafe::new_project_review(decision, root.path(), &task).unwrap();
    assert_eq!(review.decide_counted(&request()).await.unwrap().1, 1);
    bodies.recv().await.unwrap();
    db.conn
        .execute("UPDATE tasks SET status='cancelled' WHERE id=?", [&task])
        .unwrap();
    assert_eq!(
        review
            .decide_counted(&request())
            .await
            .unwrap_err()
            .attempts,
        0
    );
    assert!(!matches!(
        tokio::time::timeout(std::time::Duration::from_millis(200), bodies.recv()).await,
        Ok(Some(_))
    ));
    unsafe { std::env::remove_var(key) };
}
