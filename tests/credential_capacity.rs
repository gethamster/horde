use horde::{capacity, config::ExecutorConfig, store::Store};

#[test]
fn rotation_invalidates_provider_observations_but_preserves_budgets_and_other_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let config = ExecutorConfig {
        kind: "codex".into(),
        auth_mode: "api".into(),
        api_key_env: "ROTATION_FIXTURE_KEY".into(),
        base_url: "https://fixture.invalid/v1".into(),
        ..Default::default()
    };
    let account = capacity::account(&config);
    for (id, window, source) in [
        (account.as_str(), "weekly", "provider"),
        (account.as_str(), "budget", "local_budget"),
        ("other-account", "weekly", "provider"),
    ] {
        db.conn
            .execute(
                "INSERT INTO account_capacity VALUES(?,?,?,100,9999999999,0,?)",
                rusqlite::params![id, window, "codex", source],
            )
            .unwrap();
    }
    capacity::credentials_changed(&db, &config).unwrap();
    let rows = db
        .rows(
            "SELECT account,window FROM account_capacity ORDER BY account,window",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row["window"] == "budget"));
    assert!(rows.iter().any(|row| row["account"] == "other-account"));
}

#[test]
fn rotation_rejects_old_api_and_subscription_observations() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    for auth_mode in ["api", "login"] {
        let config = ExecutorConfig {
            kind: "codex".into(),
            auth_mode: auth_mode.into(),
            api_key_env: "ROTATION_EPOCH_FIXTURE_KEY".into(),
            base_url: "https://fixture.invalid/v1".into(),
            ..Default::default()
        };
        let old = capacity::credential_generation(&db, &config).unwrap();
        capacity::credentials_changed(&db, &config).unwrap();
        let current = capacity::credential_generation(&db, &config).unwrap();
        assert_ne!(
            old, current,
            "rotation must change an unobserved account too"
        );
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "3600".parse().unwrap());
        let event = serde_json::json!({"rate_limits":{"primary":{"used_percent":100.0,"resets_at":9999999999_i64}}});
        capacity::ingest_headers_current(&db, &config, old.as_deref(), &headers, 429).unwrap();
        capacity::ingest_current(&db, &config, old.as_deref(), &event).unwrap();
        assert!(capacity::available(&db, &capacity::account(&config)).unwrap());
        assert!(
            db.rows(
                "SELECT * FROM account_capacity WHERE account=?",
                &[&capacity::account(&config)]
            )
            .unwrap()
            .is_empty()
        );
        capacity::ingest_headers_current(&db, &config, current.as_deref(), &headers, 429).unwrap();
        capacity::ingest_current(&db, &config, current.as_deref(), &event).unwrap();
        let rows = db
            .rows(
                "SELECT window FROM account_capacity WHERE account=?",
                &[&capacity::account(&config)],
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!capacity::available(&db, &capacity::account(&config)).unwrap());
        assert_eq!(
            capacity::credential_generation(&Store::open(dir.path()).unwrap(), &config).unwrap(),
            current
        );
    }
}
