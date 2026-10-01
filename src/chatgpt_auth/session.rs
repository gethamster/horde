use super::*;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone)]
struct Session {
    root: PathBuf,
    project: String,
    account: String,
    expires_at: i64,
    request_id: Option<String>,
    timeout_seconds: i64,
    authorization_url: String,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Value>>,
}
static SESSIONS: OnceLock<Mutex<BTreeMap<String, Session>>> = OnceLock::new();
fn sessions() -> &'static Mutex<BTreeMap<String, Session>> {
    SESSIONS.get_or_init(Default::default)
}
pub fn dispatch(db: &Store, args: &Value) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    let project = crate::projects::resolve(db, args["project"].as_str().unwrap_or("default"))?;
    let account = args["account"]
        .as_str()
        .context("ChatGPT login requires a managed account")?;
    owner(db, &project, account)?;
    match args["action"].as_str().unwrap_or("start") {
        "start" => start(db, &project, account, args),
        "status" | "cancel" => {
            let id = args["session_id"].as_str().context("session_id required")?;
            let map = sessions()
                .lock()
                .map_err(|_| anyhow::anyhow!("login sessions unavailable"))?;
            let Some(session) = map.get(id) else {
                return Ok(
                    json!({"session_id":id,"status":"interrupted","reauthorization_required":true}),
                );
            };
            ensure!(
                session.root == db.root && session.project == project && session.account == account,
                "login session does not belong to selected account"
            );
            let mut result = session
                .result
                .lock()
                .map_err(|_| anyhow::anyhow!("login session unavailable"))?;
            if result["status"] == "pending" {
                if session.expires_at <= now() {
                    *result = json!({"status":"expired"});
                } else if args["action"] == "cancel" {
                    session.cancel.store(true, Ordering::SeqCst);
                    *result = json!({"status":"cancelled"});
                }
            }
            Ok(
                json!({"session_id":id,"account":account,"expires_at":session.expires_at,"status":result["status"],"result":*result}),
            )
        }
        _ => anyhow::bail!("unsupported ChatGPT login action"),
    }
}
struct Pending {
    listener: TcpListener,
    state: String,
    nonce: String,
    verifier: String,
    redirect: String,
    client_id: Option<String>,
    subject: Option<String>,
    host: String,
    version: i64,
}
fn start(db: &Store, project: &str, account: &str, args: &Value) -> Result<Value> {
    let timeout_seconds = if args["timeout_seconds"].is_null() {
        600
    } else {
        args["timeout_seconds"]
            .as_i64()
            .context("invalid login timeout")?
    };
    ensure!(
        (1..=1800).contains(&timeout_seconds),
        "login timeout must be 1..1800 seconds"
    );
    let request_id = if args["request_id"].is_null() {
        None
    } else {
        let value = args["request_id"].as_str().context("invalid request_id")?;
        ensure!(
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control),
            "invalid request_id"
        );
        Some(value.to_owned())
    };
    {
        let map = sessions()
            .lock()
            .map_err(|_| anyhow::anyhow!("login sessions unavailable"))?;
        if let Some(request) = request_id.as_deref()
            && let Some((id, saved)) = map
                .iter()
                .find(|(_, s)| s.root == db.root && s.request_id.as_deref() == Some(request))
        {
            ensure!(
                saved.project == project
                    && saved.account == account
                    && saved.timeout_seconds == timeout_seconds,
                "request_id already used for a different login"
            );
            return Ok(
                json!({"provider":"chatgpt","account":account,"session_id":id,"authorization_url":saved.authorization_url,"expires_at":saved.expires_at,"status":saved.result.lock().map_err(|_|anyhow::anyhow!("login session unavailable"))?["status"]}),
            );
        }
    }
    let _guard = lock(db, account)?;
    let saved = mapping(db, account)?;
    let host = host_id(db, account)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("ChatGPT loopback callback listener unavailable")?;
    listener.set_nonblocking(true)?;
    let redirect = format!(
        "http://127.0.0.1:{}/auth/callback",
        listener.local_addr()?.port()
    );
    let state = random()?;
    let nonce = random()?;
    let verifier = random()?;
    let client_id = saved.as_ref().map(|s| s.client_id.clone());
    let mut url = reqwest::Url::parse("https://auth.openai.com/api/accounts/authorize")?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair(
                "client_id",
                client_id.as_deref().unwrap_or("dynamic_agent_client"),
            )
            .append_pair("ext_agent_host_id", &host)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect)
            .append_pair(
                "scope",
                "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            )
            .append_pair("resource", RESOURCE)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge(&verifier));
        if saved.is_none() {
            query.append_pair("agent_name_hint", "Horde");
        } else if let Some(email) = saved.as_ref().and_then(|s| s.email.as_deref()) {
            query.append_pair("login_hint", email);
        }
    }
    // Deliberately omit retained ID token from the returned URL: login_hint still
    // supports account selection without disclosing ID tokens over MCP/RPC.
    let expires_at = now() + timeout_seconds;
    let id = crate::store::id();
    let session = Session {
        root: db.root.clone(),
        project: project.into(),
        account: account.into(),
        expires_at,
        request_id,
        timeout_seconds,
        authorization_url: url.as_str().into(),
        cancel: Arc::new(AtomicBool::new(false)),
        result: Arc::new(Mutex::new(json!({"status":"pending"}))),
    };
    {
        let mut map = sessions()
            .lock()
            .map_err(|_| anyhow::anyhow!("login sessions unavailable"))?;
        map.retain(|_, s| s.expires_at + 3600 > now());
        ensure!(map.len() < 64, "too many pending login sessions");
        ensure!(
            !map.values().any(|s| s.root == db.root
                && s.account == account
                && s.expires_at > now()
                && s.result.lock().is_ok_and(|r| r["status"] == "pending")
                && !s.cancel.load(Ordering::SeqCst)),
            "account already has a pending login"
        );
        map.insert(id.clone(), session.clone());
    }
    let pending = Pending {
        listener,
        state,
        nonce,
        verifier,
        redirect,
        client_id,
        subject: saved.map(|s| s.subject),
        host,
        version: accounts::credential_version(db, account)?,
    };
    std::thread::spawn(move || {
        let result = run(&session, pending);
        if let Ok(mut status) = session.result.lock() {
            *status = match result {
                Ok(value) => value,
                Err(_) => {
                    json!({"status":if session.cancel.load(Ordering::SeqCst){"cancelled"}else if now()>=session.expires_at {"expired"}else{"failed"},"error":"ChatGPT authorization failed; start a fresh sign-in"})
                }
            };
        }
    });
    Ok(
        json!({"provider":"chatgpt","account":account,"session_id":id,"authorization_url":url.as_str(),"expires_at":expires_at,"status":"pending"}),
    )
}
fn run(session: &Session, pending: Pending) -> Result<Value> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(session.timeout_seconds as u64)
        && now() < session.expires_at
        && !session.cancel.load(Ordering::SeqCst)
    {
        let (mut stream, peer) = match pending.listener.accept() {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let request = match request_line(&mut stream) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let first = request.lines().next().unwrap_or("");
        let mut parts = first.split_whitespace();
        let valid_method = parts.next() == Some("GET");
        let target = parts.next().unwrap_or("");
        let parsed = reqwest::Url::parse(&format!("http://127.0.0.1{target}"));
        let callback = parsed
            .ok()
            .filter(|url| valid_method && url.path() == "/auth/callback");
        let pairs = callback
            .as_ref()
            .map(|url| url.query_pairs().into_owned().collect::<Vec<_>>())
            .unwrap_or_default();
        let duplicate = pairs
            .iter()
            .enumerate()
            .any(|(i, (key, _))| pairs[..i].iter().any(|(other, _)| other == key));
        let values: BTreeMap<_, _> = pairs.into_iter().collect();
        if callback.is_none() || duplicate || values.get("state") != Some(&pending.state) {
            let _=stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 16\r\nConnection: close\r\n\r\nInvalid callback");
            continue;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let outcome = runtime.block_on(complete(session, &pending, &values));
        let message = if outcome.is_ok() {
            "ChatGPT connection complete. Return to Horde."
        } else {
            "ChatGPT connection failed. Return to Horde and retry."
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            message.len(),
            message
        );
        return outcome;
    }
    anyhow::bail!("login cancelled or expired")
}
fn request_line(stream: &mut std::net::TcpStream) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        ensure!(Instant::now() < deadline, "callback request timed out");
        let count = stream.read(&mut buffer)?;
        ensure!(count > 0, "callback request interrupted");
        bytes.extend_from_slice(&buffer[..count]);
        ensure!(bytes.len() <= 8192, "callback request too large");
        if let Some(end) = bytes.iter().position(|b| *b == b'\n') {
            return String::from_utf8(bytes[..end].to_vec())
                .context("invalid callback request encoding");
        }
    }
}
async fn complete(
    session: &Session,
    pending: &Pending,
    values: &BTreeMap<String, String>,
) -> Result<Value> {
    ensure!(
        !session.cancel.load(Ordering::SeqCst) && session.expires_at > now(),
        "login cancelled or expired"
    );
    ensure!(
        !values.contains_key("error"),
        "ChatGPT authorization denied"
    );
    let issued = callback_client(
        pending.client_id.as_deref(),
        values.get("client_id").map(String::as_str),
    )?;
    let code = values
        .get("code")
        .filter(|s| !s.is_empty() && s.len() < 4096)
        .context("authorization code missing")?;
    let http = client()?;
    let body = token(
        &http,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &issued),
            ("code", code),
            ("code_verifier", &pending.verifier),
            ("redirect_uri", &pending.redirect),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    let jwt = body["id_token"].as_str().context("ID token missing")?;
    let claims = identity(
        &http,
        jwt,
        &issued,
        Some(&pending.nonce),
        pending.subject.as_deref(),
    )
    .await?;
    let record = from_response(&body, &claims, &issued, &pending.host, None)?;
    let db = Store::open(&session.root)?;
    let _guard = lock(&db, &session.account)?;
    ensure!(
        !session.cancel.load(Ordering::SeqCst) && session.expires_at > now(),
        "login cancelled or expired"
    );
    let mut commit_status = session
        .result
        .lock()
        .map_err(|_| anyhow::anyhow!("login session unavailable"))?;
    ensure!(
        !session.cancel.load(Ordering::SeqCst) && session.expires_at > now(),
        "login cancelled or expired"
    );
    let version = save(
        &db,
        &session.project,
        &session.account,
        &record,
        pending.version,
    )?;
    let mut result = summary(&session.account, &record, version);
    result["status"] = json!("completed");
    *commit_status = result.clone();
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt_auth::integration_tests::{mock, runtime, setup, use_mock};
    #[test]
    fn complete_code_exchange_validates_and_activates_selected_account() {
        let (_temp, db, account) = setup();
        let mock = mock(false);
        use_mock(&mock);
        let session = Session {
            root: db.root.clone(),
            project: "default".into(),
            account: account.clone(),
            expires_at: now() + 60,
            request_id: None,
            timeout_seconds: 60,
            authorization_url: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(json!({"status":"pending"}))),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect = format!(
            "http://127.0.0.1:{}/auth/callback",
            listener.local_addr().unwrap().port()
        );
        let pending = Pending {
            listener,
            state: "state".into(),
            nonce: "nonce".into(),
            verifier: random().unwrap(),
            redirect,
            client_id: None,
            subject: None,
            host: host_id(&db, &account).unwrap(),
            version: 0,
        };
        let callback = BTreeMap::from([
            ("code".into(), "authorization-code".into()),
            ("client_id".into(), "oaiapp_test".into()),
        ]);
        let result = runtime()
            .block_on(complete(&session, &pending, &callback))
            .unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["plan_usage_enabled"], true);
        assert_eq!(
            loaded(&db, "default", &account).unwrap().0.access_token,
            "rotated-access"
        );
    }
    #[test]
    fn cancelled_code_exchange_never_activates_credentials() {
        let (_temp, db, account) = setup();
        let mock = mock(false);
        use_mock(&mock);
        let session = Session {
            root: db.root.clone(),
            project: "default".into(),
            account: account.clone(),
            expires_at: now() + 60,
            request_id: None,
            timeout_seconds: 60,
            authorization_url: String::new(),
            cancel: Arc::new(AtomicBool::new(true)),
            result: Arc::new(Mutex::new(json!({"status":"pending"}))),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let pending = Pending {
            listener,
            state: "state".into(),
            nonce: "nonce".into(),
            verifier: random().unwrap(),
            redirect: "http://127.0.0.1:12345/auth/callback".into(),
            client_id: None,
            subject: None,
            host: host_id(&db, &account).unwrap(),
            version: 0,
        };
        let callback = BTreeMap::from([
            ("code".into(), "authorization-code".into()),
            ("client_id".into(), "oaiapp_test".into()),
        ]);
        assert!(
            runtime()
                .block_on(complete(&session, &pending, &callback))
                .is_err()
        );
        assert_eq!(accounts::credential_version(&db, &account).unwrap(), 0);
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use crate::chatgpt_auth::integration_tests::setup;
    #[test]
    fn retry_reuses_url_but_rejects_parameter_changes() {
        let (_temp, db, account) = setup();
        let args = json!({"account":account,"request_id":"retry-id","timeout_seconds":30});
        let first = dispatch(&db, &args).unwrap();
        let second = dispatch(&db, &args).unwrap();
        assert_eq!(first["session_id"], second["session_id"]);
        assert_eq!(first["authorization_url"], second["authorization_url"]);
        assert!(
            dispatch(
                &db,
                &json!({"account":account,"request_id":"retry-id","timeout_seconds":31})
            )
            .is_err()
        );
        dispatch(
            &db,
            &json!({"account":account,"action":"cancel","session_id":first["session_id"]}),
        )
        .unwrap();
    }
    #[test]
    fn expiry_receipt_survives_and_cancel_cannot_change_completed_receipt() {
        let (_temp, db, account) = setup();
        let login = dispatch(&db, &json!({"account":account,"timeout_seconds":1})).unwrap();
        let id = login["session_id"].as_str().unwrap();
        {
            let mut map = sessions().lock().unwrap();
            map.get_mut(id).unwrap().expires_at = now() - 1;
        }
        let status = dispatch(
            &db,
            &json!({"account":account,"action":"status","session_id":id}),
        )
        .unwrap();
        assert_eq!(status["status"], "expired");
        {
            let map = sessions().lock().unwrap();
            *map.get(id).unwrap().result.lock().unwrap() = json!({"status":"completed"});
        }
        let status = dispatch(
            &db,
            &json!({"account":account,"action":"cancel","session_id":id}),
        )
        .unwrap();
        assert_eq!(status["status"], "completed");
        assert_eq!(accounts::credential_version(&db, &account).unwrap(), 0);
    }
    #[test]
    fn invalid_timeouts_never_start_a_listener() {
        let (_temp, db, account) = setup();
        for value in [json!(0), json!(1801), json!("30")] {
            assert!(dispatch(&db, &json!({"account":account,"timeout_seconds":value})).is_err());
        }
    }
}
