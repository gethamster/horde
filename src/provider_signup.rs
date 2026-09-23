//! Durable, operator-authorized Tuara signup through a Stripe Link wallet.
use crate::{config::ExecutorConfig, provider_login, store::Store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub mod cli;
mod receipt;
pub mod topup;
mod topup_wire;
mod wallet;
mod wire;
use receipt::Receipts;

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Start {
    request_id: String,
    provider: String,
    organization_name: String,
    agent_name: String,
    amount_cents: u64,
    max_charge_cents: u64,
    terms_version: String,
    accept_terms: bool,
    #[serde(default)]
    test_mode: bool,
    #[serde(default)]
    replace_existing: bool,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Start(Start),
    Status { request_id: String },
    Resume { request_id: String },
    Cancel { request_id: String },
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Preparing,
    AwaitingWallet,
    AwaitingApproval,
    Submitting,
    CredentialReceived,
    Succeeded,
    Failed,
    Cancelled,
    Uncertain,
    Expired,
}

impl Status {
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Expired
        )
    }
}

#[derive(Deserialize, Serialize)]
struct Record {
    version: u8,
    id: String,
    wallet_operation_id: String,
    root: PathBuf,
    intent: Start,
    config: ExecutorConfig,
    credential_hash: Option<String>,
    body: Value,
    status: Status,
    challenge: Option<wire::Challenge>,
    spend_request_id: Option<String>,
    wallet_status: Option<String>,
    approval_url: Option<String>,
    summary: Option<Value>,
    message: Option<String>,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn bounded(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn validate(start: &Start) -> Result<()> {
    ensure!(
        bounded(&start.request_id, 128)
            && bounded(&start.provider, 48)
            && bounded(&start.organization_name, 100)
            && bounded(&start.agent_name, 100),
        "invalid signup identity"
    );
    ensure!(
        (500..=50_000).contains(&start.amount_cents)
            && (start.amount_cents..=50_000).contains(&start.max_charge_cents),
        "fund at least 500 cents and authorize a total charge of at most 50000 cents, including fees (Link wallet limit)"
    );
    ensure!(
        start.accept_terms && bounded(&start.terms_version, 32),
        "explicit acceptance of a specific Tuara terms_version is required"
    );
    Ok(())
}

fn credential_hash(config: &ExecutorConfig) -> Result<Option<String>> {
    let path = crate::config::Settings::credentials_path();
    let mut present = std::env::var_os(&config.api_key_env).is_some();
    if path.exists() {
        ensure!(
            std::fs::metadata(&path)?.permissions().mode() & 0o077 == 0,
            "existing provider credentials must be private"
        );
        let bytes = std::fs::read_to_string(path)?;
        let values = crate::secrets::parse(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid existing provider credentials"))?;
        present |= values.contains_key(&config.api_key_env);
    }
    if !present {
        return Ok(None);
    }
    let key = crate::config::credential(&config.api_key_env)
        .map_err(|_| anyhow::anyhow!("existing provider credential unavailable"))?;
    Ok(Some(hash(key.as_bytes())))
}

fn check_target(record: &Record, new_key: Option<&str>) -> Result<()> {
    let current = provider_login::configuration(&record.intent.provider)?;
    ensure!(
        current.kind == record.config.kind
            && current.auth_mode == record.config.auth_mode
            && current.base_url == record.config.base_url
            && current.api_key_env == record.config.api_key_env
            && current.account == record.config.account,
        "provider configuration changed; restore the original signup target before resuming"
    );
    let actual = credential_hash(&current)?;
    ensure!(
        actual == record.credential_hash
            || new_key.is_some_and(|key| actual == Some(hash(key.as_bytes()))),
        "provider credentials changed during signup; existing credentials were preserved"
    );
    Ok(())
}

pub fn dispatch(db: &Store, args: &Value) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    let request: Request = serde_json::from_value(args.clone())
        .map_err(|_| anyhow::anyhow!("invalid provider signup request"))?;
    let root = db.root.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run(&root, request))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("provider signup worker failed"))?
}

async fn run(root: &Path, request: Request) -> Result<Value> {
    let receipts = Receipts::open()?;
    if let Request::Start(intent) = request {
        return start(root, &receipts, intent).await;
    }
    let (request_id, action) = match request {
        Request::Status { request_id } => (request_id, "status"),
        Request::Resume { request_id } => (request_id, "resume"),
        Request::Cancel { request_id } => (request_id, "cancel"),
        Request::Start(_) => unreachable!(),
    };
    ensure!(bounded(&request_id, 128), "invalid request_id");
    let id = hash(request_id.as_bytes());
    let mut record: Record = receipts.read(&id).context("signup request unavailable")?;
    ensure!(
        record.version == 1 && record.id == id && record.root == root,
        "signup belongs to another runtime or receipt version"
    );
    recover(&receipts, &mut record)?;
    if action == "cancel" {
        ensure!(
            matches!(
                record.status,
                Status::Preparing
                    | Status::AwaitingWallet
                    | Status::AwaitingApproval
                    | Status::Cancelled
            ),
            "a submitted or completed signup cannot be cancelled; inspect its payment outcome"
        );
        if let Some(spend_id) = record.spend_request_id.as_deref() {
            wallet::cancel(&record.root, &record.wallet_operation_id, spend_id).await?;
        }
        record.status = Status::Cancelled;
        record.message = Some(
            "Signup and its unpaid Link authorization were cancelled before payment submission."
                .into(),
        );
        receipts.write(&id, &record)?;
    } else if action == "resume" && !record.status.terminal() && record.status != Status::Uncertain
    {
        advance(&receipts, &mut record).await?;
    }
    Ok(report(&record))
}

async fn start(root: &Path, receipts: &Receipts, intent: Start) -> Result<Value> {
    validate(&intent)?;
    let id = hash(intent.request_id.as_bytes());
    if receipts.path(&id, "json").exists() {
        let mut record: Record = receipts.read(&id)?;
        ensure!(
            record.version == 1 && record.root == root && record.intent == intent,
            "request_id already used for a different signup"
        );
        recover(receipts, &mut record)?;
        return Ok(report(&record));
    }
    let config = provider_login::configuration(&intent.provider)?;
    ensure!(
        config.kind == "tuara" && config.auth_mode == "api" && config.account.is_none(),
        "signup requires an unmanaged Tuara API provider on this runtime"
    );
    provider_login::tuara::origin(&config)?;
    let credential_hash = credential_hash(&config)?;
    ensure!(
        credential_hash.is_none() || intent.replace_existing,
        "provider already has a credential; use it or explicitly authorize replace_existing"
    );
    for entry in std::fs::read_dir(&receipts.directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let prior: Record = serde_json::from_slice(&receipt::read(&path)?)
                .context("invalid private signup receipt")?;
            ensure!(
                prior.status.terminal() || prior.config.api_key_env != config.api_key_env,
                "an unresolved signup already owns this provider credential; resume that request_id before creating another"
            );
        }
    }
    let body = json!({"amount_dollars":intent.amount_cents as f64 / 100.0,
        "organization_name":intent.organization_name,"agent":{"name":intent.agent_name,"platform":"horde"},
        "terms_version":intent.terms_version});
    let mut record = Record {
        version: 1,
        id,
        wallet_operation_id: uuid::Uuid::new_v4().to_string(),
        root: root.to_owned(),
        intent,
        config,
        credential_hash,
        body,
        status: Status::Preparing,
        challenge: None,
        spend_request_id: None,
        wallet_status: None,
        approval_url: None,
        summary: None,
        message: None,
    };
    receipts.write(&record.id, &record)?;
    advance(receipts, &mut record).await?;
    Ok(report(&record))
}

fn recover(receipts: &Receipts, record: &mut Record) -> Result<()> {
    if record.status == Status::Submitting || record.status == Status::Uncertain {
        if record.status == Status::Uncertain && !receipts.path(&record.id, "response").exists() {
            return Ok(());
        }
        record.status = if receipts.path(&record.id, "response").exists() {
            Status::CredentialReceived
        } else {
            Status::Uncertain
        };
        record.message = Some(if record.status == Status::Uncertain {
            "Payment outcome is unknown. Reconcile with Tuara and your wallet; Horde will not submit another payment or create another account."
        } else { "Signup response recovered; resume to verify and install its key without another payment." }.into());
        receipts.write(&record.id, record)?;
    }
    Ok(())
}

async fn advance(receipts: &Receipts, record: &mut Record) -> Result<()> {
    let _provider_lock = provider_login::process::lock("tuara")?;
    if record.status == Status::CredentialReceived {
        return install(receipts, record).await;
    }
    check_target(record, None)?;
    let url = provider_login::tuara::origin(&record.config)?.join("/v1/agents")?;
    if record.status == Status::Preparing {
        match wire::prepare(
            &url,
            &record.body,
            record.intent.amount_cents,
            record.intent.max_charge_cents,
            &record.intent.terms_version,
        )
        .await
        {
            Ok(challenge) => {
                record.challenge = Some(challenge);
                record.status = Status::AwaitingWallet;
                record.message = None;
            }
            Err(error) => {
                record.status = Status::Failed;
                record.message = Some(format!(
                    "Tuara did not provide a valid signup quote: {error}. No payment was submitted."
                ));
            }
        }
        return receipts.write(&record.id, record);
    }
    let challenge = record
        .challenge
        .as_ref()
        .context("signup challenge missing")?;
    if crate::store::now() >= challenge.expires_at {
        if let Some(spend_id) = record.spend_request_id.as_deref() {
            wallet::cancel(&record.root, &record.wallet_operation_id, spend_id).await?;
        }
        record.status = Status::Expired;
        record.message=Some("The signup quote expired before payment submission, and its unpaid Link authorization was cancelled. Start a new request with a new request_id.".into());
        return receipts.write(&record.id, record);
    }
    if record.status == Status::AwaitingWallet {
        match wallet::create(&record.root,&record.wallet_operation_id,&challenge.network_id,challenge.charge_cents,record.intent.test_mode).await {
            Ok(spend) => {
                record.spend_request_id=Some(spend.id);
                record.wallet_status=Some(spend.status);
                record.approval_url=spend.approval_url;
                record.status=Status::AwaitingApproval;
                record.message=Some("Resume to check the existing wallet authorization. Approve its URL if the wallet requests approval.".into());
            }
            Err(_) => record.message=Some("Stripe Link wallet unavailable. Install link-cli and authenticate its wallet, then resume this same request. No Tuara payment was submitted.".into()),
        }
        return receipts.write(&record.id, record);
    }
    if record.status == Status::AwaitingApproval {
        let id = record
            .spend_request_id
            .as_deref()
            .context("wallet request missing")?;
        let spend = match wallet::retrieve(
            &record.root,
            &record.wallet_operation_id,
            id,
            &challenge.network_id,
            challenge.charge_cents,
        )
        .await
        {
            Ok(spend) => spend,
            Err(_) => {
                record.message=Some("Wallet authorization could not be checked. Resume this same request; no Tuara payment was submitted.".into());
                return receipts.write(&record.id, record);
            }
        };
        ensure!(spend.id == id, "wallet returned a different spend request");
        record.approval_url = spend.approval_url;
        record.wallet_status = Some(spend.status.clone());
        if spend.status == "requires_action" {
            record.message = Some("Stripe Link requires additional wallet action. Open your Link wallet to resolve it, then resume this same signup request. No Tuara payment was submitted.".into());
            return receipts.write(&record.id, record);
        }
        if matches!(
            spend.status.as_str(),
            "denied" | "expired" | "failed" | "canceled"
        ) {
            record.status = Status::Failed;
            record.message=Some("Wallet authorization was declined, cancelled, or expired. No Tuara payment was submitted.".into());
            return receipts.write(&record.id, record);
        }
        if spend.status != "approved" || spend.token.is_none() {
            return receipts.write(&record.id, record);
        }
        check_target(record, None)?;
        // This fsynced transition is the irreversible boundary. Never retry this POST.
        record.status = Status::Submitting;
        record.message = None;
        receipts.write(&record.id, record)?;
        let result = wire::pay(
            &url,
            &record.body,
            challenge,
            spend.token.as_deref().unwrap(),
        )
        .await;
        if let Ok(bytes) = result {
            // Save the only copy of the returned key before parsing or modifying config.
            receipts.write_bytes(&record.id, "response", &bytes)?;
            record.status = Status::CredentialReceived;
            record.message = Some(
                "Signup response saved privately. Resume to verify and install the inference key."
                    .into(),
            );
        } else if let Err(error) = result {
            record.status = Status::Uncertain;
            record.message = Some(format!(
                "Tuara payment outcome needs reconciliation: {error}. Horde will not charge again."
            ));
        }
        receipts.write(&record.id, record)?;
    }
    Ok(())
}

async fn install(receipts: &Receipts, record: &mut Record) -> Result<()> {
    let bytes = receipt::read(&receipts.path(&record.id, "response"))?;
    let parsed = wire::credential(
        &bytes,
        record
            .challenge
            .as_ref()
            .context("signup challenge missing")?,
        &record.intent.terms_version,
    );
    let (key, summary) = match parsed {
        Ok(parsed) => parsed,
        Err(_) => {
            record.message=Some("The saved signup response could not be validated. Reconcile it with Tuara; no payment will be retried.".into());
            return receipts.write(&record.id, record);
        }
    };
    check_target(record, Some(&key))?;
    let origin = provider_login::tuara::origin(&record.config)?;
    if provider_login::tuara::verify_signup(
        origin.join("/account/api/v1/auth/introspect")?,
        &key,
        summary["organization_id"]
            .as_str()
            .context("signup organization missing")?,
    )
    .await
    .is_err()
    {
        record.message=Some("The saved signup key could not be verified. Resume to retry verification without another payment.".into());
        return receipts.write(&record.id, record);
    }
    check_target(record, Some(&key))?;
    provider_login::tuara::save_key(&record.root, &record.intent.provider, &record.config, key)
        .await?;
    record.status = Status::Succeeded;
    record.summary = Some(summary);
    record.approval_url = None;
    record.message=Some("Tuara signup completed. The verified inference key is active for the next invocation; model capacity remains unknown.".into());
    receipts.write(&record.id, record)
}

fn report(record: &Record) -> Value {
    let resumable = matches!(
        record.status,
        Status::Preparing
            | Status::AwaitingWallet
            | Status::AwaitingApproval
            | Status::CredentialReceived
    );
    json!({"request_id":record.intent.request_id,"provider":record.intent.provider,"status":record.status,"test_mode":record.intent.test_mode,
        "message":record.message,"approval_url":record.approval_url,"wallet_request_id":record.spend_request_id,
        "wallet_action_required":record.wallet_status.as_deref()==Some("requires_action"),
        "quote":record.challenge.as_ref().map(|quote|json!({"amount_cents":record.intent.amount_cents,
            "charge_cents":quote.charge_cents,"max_charge_cents":record.intent.max_charge_cents,
            "currency":"usd","terms_version":record.intent.terms_version,"test_mode":record.intent.test_mode,"expires_at":quote.expires_at})),
        "result":record.summary,"provider_authentication":if record.status==Status::Succeeded {"verified"} else {"not_verified"},
        "capacity":"unknown","credential_activation":if record.status==Status::Succeeded {Some("next_invocation")} else {None},
        "next_actions":if resumable {json!([{"kind":"tool","tool":"provider_signup","arguments":{"action":"resume","request_id":record.intent.request_id}}])} else {json!([])}})
}
