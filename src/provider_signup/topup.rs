//! Operator-authorized automatic funding, independent of task execution.
use super::{
    credential_hash, hash, provider_login,
    receipt::{self, Receipts},
    topup_wire, wallet,
};
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

mod state;
use state::{Attempt, Phase, Policy, Settings};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Configure(Settings),
    Status { provider: String },
    Disable { provider: String },
    Check { provider: String },
}

pub fn dispatch(db: &Store, args: &Value) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    let request: Request = serde_json::from_value(args.clone())
        .map_err(|_| anyhow::anyhow!("invalid provider top-up request"))?;
    let root = db.root.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run(&root, request))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("provider top-up worker failed"))?
}

async fn run(root: &Path, request: Request) -> Result<Value> {
    let receipts = state::open()?;
    if let Request::Configure(settings) = request {
        return configure(root, &receipts, settings).await;
    }
    let (provider, action) = match request {
        Request::Status { provider } => (provider, "status"),
        Request::Disable { provider } => (provider, "disable"),
        Request::Check { provider } => (provider, "check"),
        Request::Configure(_) => unreachable!(),
    };
    let mut policy = state::find(&receipts, root, &provider)?;
    state::recover(&receipts, &mut policy)?;
    match action {
        "disable" => {
            policy.enabled = false;
            if policy
                .pending
                .as_ref()
                .is_some_and(|pending| pending.submitted_at.is_none())
            {
                match cancel_unsubmitted_wallet(&policy).await {
                    Ok(()) => policy.pending = None,
                    Err(_) => {
                        policy.status = "needs_attention".into();
                        policy.message = Some("Automatic top-ups disabled, but Link cancellation could not be confirmed. The unpaid authorization remains recorded for reconciliation.".into());
                    }
                }
            }
            if policy.pending.is_none() {
                policy.status = "disabled".into();
                policy.message = Some("Automatic top-ups disabled. Any unpaid Link authorization was cancelled; submitted payments remain recorded and will not be retried.".into());
            }
            receipts.write(&policy.id, &policy)?;
        }
        "check" => checked_advance(&receipts, &mut policy).await?,
        _ => {}
    }
    state::report(&receipts, &policy)
}

async fn configure(root: &Path, receipts: &Receipts, settings: Settings) -> Result<Value> {
    settings.validate()?;
    let config = provider_login::configuration(&settings.provider)?;
    ensure!(
        config.kind == "tuara" && config.auth_mode == "api" && config.account.is_none(),
        "automatic top-ups require an unmanaged Tuara API provider"
    );
    let origin = provider_login::tuara::origin(&config)?;
    let key = crate::config::credential(&config.api_key_env)
        .map_err(|_| anyhow::anyhow!("provider key unavailable; finish signup or sign in first"))?;
    let fingerprint = hash(key.as_bytes());
    ensure!(
        credential_hash(&config)? == Some(fingerprint.clone()),
        "provider credential changed"
    );
    let (organization_id, balance_units) = topup_wire::balance(&origin, &key).await?;
    let id = state::identity(&origin, &organization_id);
    for previous in state::policies(receipts)? {
        ensure!(
            previous.id == id
                || (!previous.enabled && previous.pending.is_none())
                || previous.config.api_key_env != config.api_key_env,
            "disable the old provider top-up policy before changing its organization"
        );
    }
    let previous = if receipts.path(&id, "json").exists() {
        let mut previous: Policy = receipts.read(&id)?;
        ensure!(
            previous.version == 1 && previous.root == root,
            "this organization's top-up policy belongs to another runtime"
        );
        state::recover(receipts, &mut previous)?;
        if previous.settings == settings
            && previous.credential_hash == fingerprint
            && previous.enabled
            && same_target(&previous.config, &config)
        {
            return state::report(receipts, &previous);
        }
        ensure!(
            previous.pending.is_none(),
            "an unresolved top-up already owns this organization; disable unpaid funding or reconcile a submitted payment before reconfiguring"
        );
        Some(previous)
    } else {
        None
    };
    let policy = Policy {
        version: 1,
        id,
        root: root.to_owned(),
        settings,
        config,
        credential_hash: fingerprint,
        organization_id,
        enabled: true,
        pending: None,
        next_check_at: crate::store::now(),
        balance_units: Some(balance_units),
        status: "watching".into(),
        message: Some(
            "Automatic top-ups enabled within the approved per-charge and UTC monthly limits."
                .into(),
        ),
        last_payment: previous.and_then(|previous| previous.last_payment),
    };
    receipts.write(&policy.id, &policy)?;
    state::report(receipts, &policy)
}

/// The daemon calls this independently of scheduling. The private global lock
/// serializes aliases and runtimes sharing one credential configuration directory.
pub async fn tick(root: &Path) -> Result<()> {
    if !state::directory().exists() {
        return Ok(());
    }
    let receipts = state::open()?;
    for mut policy in state::policies(&receipts)? {
        if policy.root != root || policy.next_check_at > crate::store::now() {
            continue;
        }
        state::recover(&receipts, &mut policy)?;
        if policy.enabled
            || policy
                .pending
                .as_ref()
                .is_some_and(|pending| pending.phase == Phase::Received)
        {
            checked_advance(&receipts, &mut policy).await?;
        }
    }
    Ok(())
}

async fn checked_advance(receipts: &Receipts, policy: &mut Policy) -> Result<()> {
    policy.next_check_at = crate::store::now() + 60;
    let result = advance(receipts, policy).await;
    if result.is_err() {
        // Never return transport bodies, credentials, or wallet output. A
        // submitting transition must retain its uncertainty even on local I/O failure.
        state::recover(receipts, policy)?;
        if policy
            .pending
            .as_ref()
            .is_none_or(|pending| !matches!(pending.phase, Phase::Uncertain | Phase::Received))
        {
            policy.status = "needs_attention".into();
            policy.message = Some("Top-up check could not verify the provider, balance, budget, or wallet. No payment will be retried; check the configured key and Link wallet.".into());
        }
    }
    receipts.write(&policy.id, policy)
}

fn key(policy: &Policy) -> Result<String> {
    let config = provider_login::configuration(&policy.settings.provider)?;
    ensure!(
        same_target(&config, &policy.config),
        "top-up provider configuration changed"
    );
    ensure!(
        credential_hash(&config)? == Some(policy.credential_hash.clone()),
        "top-up credential changed; explicitly configure the new identity"
    );
    crate::config::credential(&config.api_key_env)
        .map_err(|_| anyhow::anyhow!("top-up provider key unavailable"))
}

fn same_target(a: &crate::config::ExecutorConfig, b: &crate::config::ExecutorConfig) -> bool {
    a.kind == b.kind
        && a.auth_mode == b.auth_mode
        && a.base_url == b.base_url
        && a.api_key_env == b.api_key_env
        && a.account == b.account
}

async fn balance(policy: &mut Policy, key: &str) -> Result<i64> {
    let origin = provider_login::tuara::origin(&policy.config)?;
    let (organization, balance) = topup_wire::balance(&origin, key).await?;
    ensure!(
        organization == policy.organization_id,
        "top-up organization changed"
    );
    policy.balance_units = Some(balance);
    Ok(balance)
}

async fn advance(receipts: &Receipts, policy: &mut Policy) -> Result<()> {
    let _provider_lock = provider_login::process::lock("tuara")?;
    if policy
        .pending
        .as_ref()
        .is_some_and(|pending| pending.phase == Phase::Received)
    {
        return finish(receipts, policy);
    }
    if !policy.enabled
        || policy
            .pending
            .as_ref()
            .is_some_and(|pending| pending.phase == Phase::Uncertain)
    {
        return Ok(());
    }
    let key = key(policy)?;
    let origin = provider_login::tuara::origin(&policy.config)?;
    let url = origin.join("/v1/account/topup")?;
    if policy.pending.is_none() {
        let available = balance(policy, &key).await?;
        if !policy.settings.low(available) {
            policy.status = "watching".into();
            policy.message = Some("Balance is above the top-up threshold.".into());
            return Ok(());
        }
        if !state::room(
            receipts,
            policy,
            policy.settings.amount_cents,
            crate::store::now(),
        )? {
            budget_hold(policy);
            return Ok(());
        }
        let body = json!({"amount_dollars":policy.settings.amount_cents as f64/100.0});
        let challenge = topup_wire::prepare_topup(
            &url,
            &key,
            &body,
            policy.settings.amount_cents,
            policy.settings.max_charge_cents,
            &policy.settings.terms_version,
        )
        .await?;
        if !state::room(
            receipts,
            policy,
            challenge.charge_cents,
            crate::store::now(),
        )? {
            budget_hold(policy);
            return Ok(());
        }
        policy.pending = Some(Attempt {
            id: uuid::Uuid::new_v4().to_string(),
            body,
            challenge,
            phase: Phase::Wallet,
            spend_id: None,
            approval_url: None,
            wallet_status: None,
            submitted_at: None,
        });
        policy.status = "awaiting_wallet".into();
        policy.message = None;
        return Ok(());
    }
    let pending = policy
        .pending
        .as_ref()
        .context("top-up operation missing")?;
    if pending.challenge.expires_at <= crate::store::now() {
        cancel_unsubmitted_wallet(policy).await?;
        policy.pending = None;
        policy.status = "watching".into();
        policy.message=Some("Unpaid quote expired and its Link authorization was cancelled. A later balance check may request a new quote within the same limits.".into());
        return Ok(());
    }
    if pending.phase == Phase::Wallet {
        let spend = wallet::create_topup(
            &policy.root,
            &pending.id,
            &pending.challenge.network_id,
            pending.challenge.charge_cents,
            policy.settings.test_mode,
        )
        .await?;
        let pending = policy.pending.as_mut().unwrap();
        pending.spend_id = Some(spend.id);
        pending.approval_url = spend.approval_url;
        pending.wallet_status = Some(spend.status);
        pending.phase = Phase::Approval;
        policy.status = "awaiting_approval".into();
        policy.message=Some("Waiting for the existing Link wallet authorization. Open its approval URL or resolve any required wallet action.".into());
        return Ok(());
    }
    let spend = wallet::retrieve(
        &policy.root,
        &pending.id,
        pending
            .spend_id
            .as_deref()
            .context("top-up wallet request missing")?,
        &pending.challenge.network_id,
        pending.challenge.charge_cents,
    )
    .await?;
    let pending = policy.pending.as_mut().unwrap();
    pending.approval_url = spend.approval_url;
    pending.wallet_status = Some(spend.status.clone());
    if matches!(
        spend.status.as_str(),
        "denied" | "expired" | "failed" | "canceled"
    ) {
        policy.enabled = false;
        policy.pending = None;
        policy.status = "disabled".into();
        policy.message=Some("Wallet authorization was declined, cancelled, or expired. Configure top-ups again to re-enable them.".into());
        return Ok(());
    }
    if spend.status != "approved" || spend.token.is_none() {
        return Ok(());
    }
    let available = balance(policy, &key).await?;
    if !policy.settings.low(available) {
        cancel_unsubmitted_wallet(policy).await?;
        policy.pending = None;
        policy.status = "watching".into();
        policy.message = Some(
            "Balance recovered before payment; this top-up was cancelled without charging.".into(),
        );
        return Ok(());
    }
    ensure!(
        self::key(policy)? == key,
        "top-up key changed before payment"
    );
    let pending = policy.pending.as_ref().unwrap();
    if pending.challenge.expires_at <= crate::store::now() {
        cancel_unsubmitted_wallet(policy).await?;
        policy.pending = None;
        policy.status = "watching".into();
        return Ok(());
    }
    if !state::room(
        receipts,
        policy,
        pending.challenge.charge_cents,
        crate::store::now(),
    )? {
        cancel_unsubmitted_wallet(policy).await?;
        policy.pending = None;
        budget_hold(policy);
        return Ok(());
    }
    let pending = policy.pending.as_mut().unwrap();
    pending.phase = Phase::Submitting;
    pending.submitted_at = Some(crate::store::now());
    policy.status = "submitting".into();
    policy.message = None;
    receipts.write(&policy.id, policy)?;
    let pending = policy.pending.as_ref().unwrap();
    match topup_wire::pay_topup(
        &url,
        &key,
        &pending.body,
        &pending.challenge,
        spend.token.as_deref().unwrap(),
    )
    .await
    {
        Ok(bytes) => {
            receipts.write_bytes(&pending.id, "response", &bytes)?;
            policy.pending.as_mut().unwrap().phase = Phase::Received;
            policy.status = "payment_received".into();
            policy.message=Some("Payment response saved privately; the next check records the funded balance without paying again.".into());
        }
        Err(_) => {
            policy.pending.as_mut().unwrap().phase = Phase::Uncertain;
            policy.status = "uncertain".into();
            policy.message=Some("Payment outcome is unknown. Reconcile with Tuara and Link; automatic charges are held.".into());
        }
    }
    Ok(())
}

/// Do not discard a Link authorization while it can still be spent. If
/// cancellation cannot be confirmed, retain the pending attempt for recovery.
async fn cancel_unsubmitted_wallet(policy: &Policy) -> Result<()> {
    let Some(pending) = policy.pending.as_ref() else {
        return Ok(());
    };
    if pending.submitted_at.is_some() {
        return Ok(());
    }
    if let Some(spend_id) = pending.spend_id.as_deref() {
        wallet::cancel(&policy.root, &pending.id, spend_id).await?;
    }
    Ok(())
}

fn budget_hold(policy: &mut Policy) {
    policy.status = "budget_exhausted".into();
    policy.message=Some("The remaining UTC monthly budget cannot cover this top-up including fees. No payment was submitted.".into());
}

fn finish(receipts: &Receipts, policy: &mut Policy) -> Result<()> {
    let pending = policy.pending.as_ref().context("saved top-up missing")?;
    let bytes = receipt::read(&receipts.path(&pending.id, "response"))?;
    let summary = match topup_wire::validate_topup_response(&bytes, &pending.challenge) {
        Ok(summary) => summary,
        Err(_) => {
            policy.status = "needs_attention".into();
            policy.message=Some("The saved payment response could not be validated. Reconcile with Tuara; payment will not be retried.".into());
            return Ok(());
        }
    };
    // Ledger first, then clear pending. Replaying after a crash writes the same
    // operation ID, and spent() deduplicates that ID against the pending record.
    state::settle(receipts, policy)?;
    policy.last_payment = Some(summary);
    policy.pending = None;
    policy.status = if policy.enabled {
        "watching"
    } else {
        "disabled"
    }
    .into();
    policy.message =
        Some("Tuara top-up completed. Balance monitoring will resume after five minutes.".into());
    policy.next_check_at = crate::store::now() + 300;
    Ok(())
}
