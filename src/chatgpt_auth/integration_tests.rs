use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
pub(super) fn fixture() -> Value {
    serde_json::from_str(include_str!("test_identity.json")).unwrap()
}
pub(super) fn setup() -> (tempfile::TempDir, Store, String) {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open(temp.path()).unwrap();
    let account=accounts::dispatch(&db,"account_create",&json!({"project":"default","name":"chatgpt-test","provider":"chatgpt","auth_mode":"login","base_url":"https://api.openai.com/v1"})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    let shared = root(&db, &account).unwrap().parent().unwrap().to_owned();
    write_private_atomic(
        &shared.join("host-id"),
        b"urn:uuid:de31b716-84d7-4852-a6da-3bb1206d6baa",
    )
    .unwrap();
    (temp, db, account)
}
fn registration() -> Registration {
    let mut r = tests::sample();
    r.ext_agent_host_id = "urn:uuid:de31b716-84d7-4852-a6da-3bb1206d6baa".into();
    r.id_token = fixture()["valid"].as_str().unwrap().into();
    r
}
pub(super) struct Mock {
    base: String,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
        TEST_SERVICE.with(|service| *service.borrow_mut() = None);
    }
}
pub(super) fn mock(token_error: bool) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stopping.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(s) => s,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let n = match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                bytes.extend_from_slice(&buffer[..n]);
                let request = String::from_utf8_lossy(&bytes);
                if let Some(header_end) = request.find("\r\n\r\n") {
                    let length = request[..header_end]
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + length {
                        break;
                    }
                }
                if bytes.len() > 64 * 1024 {
                    break;
                }
            }
            if bytes.is_empty() {
                continue;
            }
            let request = String::from_utf8_lossy(&bytes);
            let (status, body) = if request.contains("/openid-configuration") {
                (
                    "200 OK",
                    json!({"issuer":ISSUER,"jwks_uri":"https://auth.openai.com/.well-known/jwks.json","revocation_endpoint":"https://auth.openai.com/api/accounts/oauth/revoke"}),
                )
            } else if request.contains("/jwks.json") {
                ("200 OK", fixture()["jwks"].clone())
            } else if request.contains("/oauth/token") {
                count.fetch_add(1, Ordering::SeqCst);
                if token_error {
                    ("400 Bad Request", json!({"error":"invalid_grant"}))
                } else {
                    (
                        "200 OK",
                        json!({"id_token":fixture()["valid"],"access_token":"rotated-access","refresh_token":"rotated-refresh","token_type":"Bearer","expires_in":3600,"scope":"openid resource.invoke chatgpt.tokens.use.direct"}),
                    )
                }
            } else {
                ("200 OK", json!({}))
            };
            let text = body.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    Mock {
        base,
        calls,
        stop,
        handle: Some(handle),
    }
}
pub(super) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
pub(super) fn use_mock(mock: &Mock) {
    TEST_SERVICE.with(|service| *service.borrow_mut() = Some(mock.base.clone()));
}
#[test]
fn validates_signed_identity_all_claim_boundaries() {
    let f = fixture();
    assert!(
        validate_jwt(
            f["valid"].as_str().unwrap(),
            &f["jwks"],
            "oaiapp_test",
            Some("nonce"),
            Some("subject")
        )
        .is_ok()
    );
    for name in [
        "expired",
        "issuer",
        "audience",
        "nonce",
        "subject",
        "not_before",
    ] {
        assert!(
            validate_jwt(
                f[name].as_str().unwrap(),
                &f["jwks"],
                "oaiapp_test",
                Some("nonce"),
                Some("subject")
            )
            .is_err(),
            "accepted {name}"
        );
    }
    let mut signature = f["valid"].as_str().unwrap().as_bytes().to_vec();
    let n = signature.len();
    signature[n - 10] = if signature[n - 10] == b'A' {
        b'B'
    } else {
        b'A'
    };
    assert!(
        validate_jwt(
            std::str::from_utf8(&signature).unwrap(),
            &f["jwks"],
            "oaiapp_test",
            None,
            None
        )
        .is_err()
    );
}
#[test]
fn refresh_rotates_once_for_concurrent_requests_and_preserves_mapping() {
    let (_temp, db, account) = setup();
    let mut r = registration();
    r.expires_at = now() - 1;
    save(&db, "default", &account, &r, 0).unwrap();
    let mock = mock(false);
    use_mock(&mock);
    runtime().block_on(async {
        let (one, two) = tokio::join!(
            access_token(&db, "default", &account),
            access_token(&db, "default", &account)
        );
        assert_eq!(one.unwrap(), "rotated-access");
        assert_eq!(two.unwrap(), "rotated-access");
    });
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let (saved, version) = loaded(&db, "default", &account).unwrap();
    assert_eq!(saved.refresh_token, "rotated-refresh");
    assert_eq!(version, 2);
    assert_eq!(mapping(&db, &account).unwrap().unwrap().subject, "subject");
}
#[test]
fn terminal_refresh_clears_tokens_and_retains_registration() {
    let (_temp, db, account) = setup();
    let mut r = registration();
    r.expires_at = now() - 1;
    save(&db, "default", &account, &r, 0).unwrap();
    let profile: String = db
        .conn
        .query_row(
            "SELECT id FROM auth_profiles WHERE account=?",
            [&account],
            |r| r.get(0),
        )
        .unwrap();
    db.conn.execute("INSERT INTO account_remote_reservations VALUES('request','default',?,'remote','task','attempt','role',?,1,'active',0)",rusqlite::params![account,profile]).unwrap();
    db.conn.execute("INSERT INTO credential_deliveries VALUES('request','default',?,'remote',1,'delivered',0)",[&account]).unwrap();
    let mock = mock(true);
    use_mock(&mock);
    assert!(
        runtime()
            .block_on(access_token(&db, "default", &account))
            .is_err()
    );
    assert!(loaded(&db, "default", &account).is_err());
    assert_eq!(
        db.conn
            .query_row(
                "SELECT state FROM account_remote_reservations WHERE request_id='request'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "revoked"
    );
    assert_eq!(
        db.conn
            .query_row(
                "SELECT state FROM credential_deliveries WHERE request_id='request'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "revocation_pending"
    );
    assert!(mapping(&db, &account).unwrap().is_some());
    assert!(
        !db.root
            .join("private/accounts")
            .join(&account)
            .join("1.json")
            .exists()
    );
}
#[test]
fn import_validates_identity_preserves_destination_host_and_rejects_wrong_account() {
    let (_temp, db, account) = setup();
    let destination = prepare_remote(&db, "default", &account).unwrap()["ext_agent_host_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mock = mock(false);
    use_mock(&mock);
    let r = registration();
    let result = runtime()
        .block_on(import(
            &db,
            "default",
            &account,
            serde_json::to_value(&r).unwrap(),
        ))
        .unwrap();
    assert_eq!(result["subject"], "subject");
    assert_eq!(
        loaded(&db, "default", &account)
            .unwrap()
            .0
            .ext_agent_host_id,
        destination
    );
    let mut wrong = r;
    wrong.subject = "different".into();
    assert!(
        runtime()
            .block_on(import(
                &db,
                "default",
                &account,
                serde_json::to_value(wrong).unwrap()
            ))
            .is_err()
    );
    assert_eq!(loaded(&db, "default", &account).unwrap().1, 1);
}
#[test]
fn signout_clears_tokens_revokes_session_and_reuses_client() {
    let (_temp, db, account) = setup();
    let r = registration();
    save(&db, "default", &account, &r, 0).unwrap();
    let mock = mock(false);
    use_mock(&mock);
    let result = runtime()
        .block_on(sign_out(&db, "default", &account))
        .unwrap();
    assert_eq!(result["remote_revocation_confirmed"], true);
    assert!(loaded(&db, "default", &account).is_err());
    assert_eq!(
        mapping(&db, &account).unwrap().unwrap().client_id,
        "oaiapp_test"
    );
    let login = dispatch(&db, &json!({"account":account})).unwrap();
    assert!(
        login["authorization_url"]
            .as_str()
            .unwrap()
            .contains("client_id=oaiapp_test")
    );
    assert!(!login.to_string().contains(&r.id_token));
    dispatch(
        &db,
        &json!({"account":account,"action":"cancel","session_id":login["session_id"]}),
    )
    .unwrap();
}
#[test]
fn login_binds_loopback_fresh_pkce_and_cancellation_without_replacing_account() {
    let (_temp, db, account) = setup();
    let r = registration();
    save(&db, "default", &account, &r, 0).unwrap();
    let login = dispatch(&db, &json!({"account":account})).unwrap();
    let url = reqwest::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(query["redirect_uri"].ends_with("/auth/callback"));
    assert!(!query.contains_key("agent_name_hint"));
    assert!(!query.contains_key("id_token_hint"));
    let status = dispatch(
        &db,
        &json!({"account":account,"action":"cancel","session_id":login["session_id"]}),
    )
    .unwrap();
    assert_eq!(status["status"], "cancelled");
    assert_eq!(loaded(&db, "default", &account).unwrap().1, 1);
}
#[test]
fn credentials_and_mapping_are_private_atomic_and_isolated() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, db, account) = setup();
    let r = registration();
    save(&db, "default", &account, &r, 0).unwrap();
    let private = db
        .root
        .join("private/accounts")
        .join(&account)
        .join("1.json");
    assert_eq!(
        std::fs::metadata(private).unwrap().permissions().mode() & 0o077,
        0
    );
    let mut other = r.clone();
    other.client_id = "oaiapp_other".into();
    assert!(save(&db, "default", &account, &other, 1).is_err());
    assert_eq!(loaded(&db, "default", &account).unwrap().1, 1);
    let path = root(&db, &account).unwrap().join("overwrite");
    write_private_atomic(&path, b"old").unwrap();
    write_private_atomic(&path, b"new").unwrap();
    assert_eq!(read_private(&path).unwrap(), b"new");
    let symlink = root(&db, &account).unwrap().join("link");
    std::os::unix::fs::symlink(&path, &symlink).unwrap();
    assert!(write_private_atomic(&symlink, b"bad").is_err());
    assert!(read_private(&symlink).is_err());
}
#[test]
fn export_transfers_refresh_ownership_and_never_overwrites_bundle() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, db, account) = setup();
    let r = registration();
    save(&db, "default", &account, &r, 0).unwrap();
    let path = temp.path().join("transfer.json");
    let result = export_file(&db, "default", &account, &path).unwrap();
    assert_eq!(result["refresh_owner"], "destination");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
        0
    );
    assert!(loaded(&db, "default", &account).is_err());
    assert!(mapping(&db, &account).unwrap().is_some());
    let transferred: Registration = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(transferred.refresh_token, r.refresh_token);
    assert!(!result.to_string().contains(&r.refresh_token));
    assert!(export_file(&db, "default", &account, &path).is_err());
}
#[test]
fn rejects_untrusted_discovery_endpoints() {
    for uri in [
        "http://auth.openai.com/jwks",
        "https://attacker.invalid/jwks",
        "https://auth.openai.com:8443/jwks",
        "https://user:pass@auth.openai.com/jwks",
        "https://auth.openai.com/jwks?token=x",
    ] {
        assert!(trusted_endpoint(&json!({"jwks_uri":uri}), "jwks_uri").is_err());
    }
    assert!(
        trusted_endpoint(
            &json!({"jwks_uri":"https://auth.openai.com/custom-jwks"}),
            "jwks_uri"
        )
        .is_ok()
    );
}

#[test]
fn runtime_host_identity_is_shared_across_accounts() {
    let (_temp, db, account) = setup();
    let one = host_id(&db, &account).unwrap();
    let two = accounts::dispatch(
        &db,
        "account_create",
        &json!({"name":"second","provider":"chatgpt","auth_mode":"login","base_url":RESOURCE}),
    )
    .unwrap()
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(host_id(&db, &two).unwrap(), one);
}
#[test]
fn temporary_exchange_failure_preserves_credentials_and_mapping() {
    let (_temp, db, account) = setup();
    let mut record = registration();
    record.expires_at = now() - 1;
    save(&db, "default", &account, &record, 0).unwrap();
    let mock = mock(false);
    use_mock(&mock);
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let unavailable = format!("http://{}", socket.local_addr().unwrap());
    drop(socket);
    TEST_SERVICE.with(|service| *service.borrow_mut() = Some(unavailable));
    assert!(
        runtime()
            .block_on(access_token(&db, "default", &account))
            .is_err()
    );
    let (current, version) = loaded(&db, "default", &account).unwrap();
    assert_eq!(version, 1);
    assert_eq!(current.refresh_token, record.refresh_token);
    assert!(mapping(&db, &account).unwrap().is_some());
}
#[test]
fn missing_plan_permission_and_early_refresh_never_exchange_tokens() {
    let (_temp, db, account) = setup();
    let mut record = registration();
    record.scopes = vec!["openid".into()];
    save(&db, "default", &account, &record, 0).unwrap();
    let mock = mock(false);
    use_mock(&mock);
    assert!(
        runtime()
            .block_on(access_token(&db, "default", &account))
            .is_err()
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    record.scopes = vec!["resource.invoke".into(), "chatgpt.tokens.use.direct".into()];
    record.expires_at = now() - 1;
    record.earliest_refresh_at = Some(now() + 60);
    save(&db, "default", &account, &record, 1).unwrap();
    assert!(
        runtime()
            .block_on(access_token(&db, "default", &account))
            .is_err()
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    assert_eq!(loaded(&db, "default", &account).unwrap().1, 2);
}
#[test]
fn managed_account_grants_protect_tokens_and_login_receipts() {
    let (_temp, db, account) = setup();
    let record = registration();
    save(&db, "default", &account, &record, 0).unwrap();
    assert!(
        runtime()
            .block_on(access_token(&db, "ungranted", &account))
            .is_err()
    );
    assert!(export(&db, "ungranted", &account).is_err());
    let login = dispatch(&db, &json!({"account":account})).unwrap();
    let second = accounts::dispatch(
        &db,
        "account_create",
        &json!({"name":"second","provider":"chatgpt","auth_mode":"login","base_url":RESOURCE}),
    )
    .unwrap()
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        dispatch(
            &db,
            &json!({"account":second,"action":"status","session_id":login["session_id"]})
        )
        .is_err()
    );
    dispatch(
        &db,
        &json!({"account":account,"action":"cancel","session_id":login["session_id"]}),
    )
    .unwrap();
}
#[test]
fn loopback_rejects_wrong_state_and_keeps_active_credentials() {
    let (_temp, db, account) = setup();
    let record = registration();
    save(&db, "default", &account, &record, 0).unwrap();
    let login = dispatch(&db, &json!({"account":account})).unwrap();
    let url = reqwest::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    let callback = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    let status = runtime().block_on(async {
        client()
            .unwrap()
            .get(format!(
                "{callback}?state=wrong&code=ignored&client_id=oaiapp_test"
            ))
            .send()
            .await
            .unwrap()
            .status()
    });
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(loaded(&db, "default", &account).unwrap().1, 1);
    dispatch(
        &db,
        &json!({"account":account,"action":"cancel","session_id":login["session_id"]}),
    )
    .unwrap();
}
#[test]
fn failed_export_gates_source_and_can_retry_retained_private_session() {
    let (temp, db, account) = setup();
    let record = registration();
    save(&db, "default", &account, &record, 0).unwrap();
    let occupied = temp.path().join("occupied.json");
    std::fs::write(&occupied, b"existing").unwrap();
    assert!(export_file(&db, "default", &account, &occupied).is_err());
    let authenticated: bool = db
        .conn
        .query_row(
            "SELECT authenticated=1 FROM accounts WHERE id=?",
            [&account],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !authenticated,
        "failed handoff must durably gate the source before making a bundle available"
    );
    assert!(
        runtime()
            .block_on(access_token(&db, "default", &account))
            .is_err()
    );
    let paused_version = accounts::credential_version(&db, &account).unwrap();
    assert!(
        paused_version > 1,
        "handoff invalidates pending authorization generations"
    );
    assert!(
        accounts::credential(&db, "default", &account).is_ok(),
        "failed output retains protected session for explicit retry"
    );
    assert_eq!(std::fs::read(&occupied).unwrap(), b"existing");
    drop(db);
    let db = Store::open(temp.path()).unwrap();
    let retry = temp.path().join("retry.json");
    let result = export_file(&db, "default", &account, &retry).unwrap();
    assert_eq!(result["refresh_owner"], "destination");
    let restored: Registration = serde_json::from_slice(&std::fs::read(retry).unwrap()).unwrap();
    assert_eq!(restored.refresh_token, record.refresh_token);
    assert!(accounts::credential(&db, "default", &account).is_err());
}
