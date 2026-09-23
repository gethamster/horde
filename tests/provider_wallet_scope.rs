use horde::{protocol, store::Store};
use serde_json::json;

#[test]
fn wallet_onboarding_is_admin_only_and_publishes_bounded_actions() {
    let directory = tempfile::tempdir().unwrap();
    let db = Store::open(directory.path()).unwrap();
    assert!(!protocol::worker_allowed("provider_wallet"));
    assert!(!protocol::project_allowed("provider_wallet"));
    let args = json!({"action":"inspect"});
    assert!(
        protocol::dispatch_scoped(&db, "provider_wallet", args.clone(), None, Some("default"))
            .is_err()
    );
    assert!(
        protocol::dispatch(
            &db,
            "provider_wallet",
            args.clone(),
            Some("invalid-worker-token")
        )
        .is_err()
    );
    assert!(
        protocol::dispatch(
            &db,
            "provider_wallet",
            json!({"action":"install","request_id":"scope-test","project":"other"}),
            None
        )
        .is_err()
    );
    let schema = protocol::schema("provider_wallet");
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("install"))
    );
    for secret in ["card_number", "cvc", "access_token", "password"] {
        assert!(schema["properties"].get(secret).is_none());
    }
}
