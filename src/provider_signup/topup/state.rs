use super::super::{
    hash,
    receipt::{self, Receipts},
    wire,
};
use crate::config::ExecutorConfig;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    pub provider: String,
    pub threshold_cents: u64,
    pub amount_cents: u64,
    pub max_charge_cents: u64,
    pub monthly_limit_cents: u64,
    pub terms_version: String,
    pub accept_terms: bool,
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            super::super::bounded(&self.provider, 48),
            "invalid provider name"
        );
        ensure!(
            (1..=50_000).contains(&self.threshold_cents),
            "balance threshold must be between 1 and 50000 cents"
        );
        ensure!(
            (500..=50_000).contains(&self.amount_cents),
            "top-up credit must be at least 500 cents and at most 50000 cents"
        );
        ensure!(
            (self.amount_cents..=50_000).contains(&self.max_charge_cents),
            "authorize the credit plus fees, at most 50000 cents per charge"
        );
        ensure!(
            (self.max_charge_cents..=100_000_000).contains(&self.monthly_limit_cents),
            "monthly charge limit must cover one maximum charge and be at most 100000000 cents"
        );
        ensure!(
            self.accept_terms && super::super::bounded(&self.terms_version, 32),
            "explicit acceptance of a specific terms version and recurring charge limits is required"
        );
        Ok(())
    }

    pub fn low(&self, available_units: i64) -> bool {
        available_units < (self.threshold_cents * 1_000_000) as i64
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Wallet,
    Approval,
    Submitting,
    Received,
    Uncertain,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Attempt {
    pub id: String,
    pub body: Value,
    pub challenge: wire::Challenge,
    pub phase: Phase,
    pub spend_id: Option<String>,
    pub approval_url: Option<String>,
    pub wallet_status: Option<String>,
    pub submitted_at: Option<i64>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Policy {
    pub version: u8,
    pub id: String,
    pub root: PathBuf,
    pub settings: Settings,
    pub config: ExecutorConfig,
    pub credential_hash: String,
    pub organization_id: String,
    pub enabled: bool,
    pub pending: Option<Attempt>,
    pub next_check_at: i64,
    pub balance_units: Option<i64>,
    pub status: String,
    pub message: Option<String>,
    pub last_payment: Option<Value>,
}

#[derive(Deserialize, Serialize)]
struct Charge {
    id: String,
    submitted_at: i64,
    cents: u64,
}

pub(super) fn directory() -> PathBuf {
    crate::branding::config_dir().join("provider-topups")
}

pub(super) fn open() -> Result<Receipts> {
    std::fs::create_dir_all(crate::branding::config_dir())?;
    Receipts::at(&directory())
}

pub(super) fn policies(receipts: &Receipts) -> Result<Vec<Policy>> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(&receipts.directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let policy: Policy = serde_json::from_slice(&receipt::read(&path)?)
                .map_err(|_| anyhow::anyhow!("invalid private top-up policy"))?;
            ensure!(
                policy.version == 1 && path == receipts.path(&policy.id, "json"),
                "unsupported top-up policy"
            );
            result.push(policy);
        }
    }
    Ok(result)
}

pub(super) fn find(receipts: &Receipts, root: &Path, provider: &str) -> Result<Policy> {
    ensure!(super::super::bounded(provider, 48), "invalid provider name");
    let config = crate::provider_login::configuration(provider).ok();
    let mut matching = policies(receipts)?.into_iter().filter(|policy| {
        policy.settings.provider == provider
            || config.as_ref().is_some_and(|config| {
                config.api_key_env == policy.config.api_key_env
                    && config.base_url == policy.config.base_url
            })
    });
    let policy = matching
        .next()
        .context("no automatic top-up policy; configure this provider first")?;
    ensure!(
        matching.next().is_none(),
        "ambiguous provider top-up policies; use their configured names"
    );
    ensure!(
        policy.root == root,
        "top-up policy belongs to another runtime"
    );
    Ok(policy)
}

pub(super) fn identity(origin: &reqwest::Url, organization: &str) -> String {
    hash(format!("{}\n{organization}", origin.origin().ascii_serialization()).as_bytes())
}

fn month_start(now: i64) -> Result<i64> {
    let date = time::OffsetDateTime::from_unix_timestamp(now)?.date();
    Ok(
        time::Date::from_calendar_date(date.year(), date.month(), 1)?
            .midnight()
            .assume_utc()
            .unix_timestamp(),
    )
}

fn ledger(receipts: &Receipts, policy: &Policy) -> Result<Receipts> {
    Receipts::at(&receipts.directory.join(format!("{}-charges", policy.id)))
}

pub(super) fn spent(receipts: &Receipts, policy: &Policy, now: i64) -> Result<u64> {
    let start = month_start(now)?;
    let ledger = ledger(receipts, policy)?;
    let mut total = 0u64;
    let mut pending_recorded = false;
    for entry in std::fs::read_dir(&ledger.directory)? {
        let path = entry?.path();
        if !path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            continue;
        }
        let charge: Charge = serde_json::from_slice(&receipt::read(&path)?)
            .map_err(|_| anyhow::anyhow!("invalid top-up charge ledger"))?;
        if policy
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == charge.id)
        {
            pending_recorded = true;
        }
        if charge.submitted_at >= start {
            total = total
                .checked_add(charge.cents)
                .context("top-up charge total overflow")?;
        }
    }
    if !pending_recorded
        && let Some(pending) = &policy.pending
        && pending
            .submitted_at
            .is_some_and(|timestamp| timestamp >= start)
    {
        total = total
            .checked_add(pending.challenge.charge_cents)
            .context("top-up charge total overflow")?;
    }
    Ok(total)
}

pub(super) fn room(receipts: &Receipts, policy: &Policy, cents: u64, now: i64) -> Result<bool> {
    Ok(spent(receipts, policy, now)?
        .checked_add(cents)
        .is_some_and(|total| total <= policy.settings.monthly_limit_cents))
}

pub(super) fn settle(receipts: &Receipts, policy: &Policy) -> Result<()> {
    let pending = policy.pending.as_ref().context("top-up receipt missing")?;
    let charge = Charge {
        id: pending.id.clone(),
        submitted_at: pending
            .submitted_at
            .context("top-up submission time missing")?,
        cents: pending.challenge.charge_cents,
    };
    ledger(receipts, policy)?.write(&charge.id, &charge)
}

pub(super) fn recover(receipts: &Receipts, policy: &mut Policy) -> Result<()> {
    if let Some(pending) = &mut policy.pending
        && matches!(pending.phase, Phase::Submitting | Phase::Uncertain)
    {
        if receipts.path(&pending.id, "response").exists() {
            pending.phase = Phase::Received;
            policy.status = "payment_received".into();
            policy.message = Some(
                "Saved payment response recovered; recording it will not charge again.".into(),
            );
        } else {
            pending.phase = Phase::Uncertain;
            policy.status = "uncertain".into();
            policy.message = Some("Payment outcome is unknown. Reconcile with Tuara and Link; automatic charges are held.".into());
        }
        receipts.write(&policy.id, policy)?;
    }
    Ok(())
}

pub(super) fn report(receipts: &Receipts, policy: &Policy) -> Result<Value> {
    let spent = spent(receipts, policy, crate::store::now())?;
    let reserved = policy
        .pending
        .as_ref()
        .filter(|pending| pending.submitted_at.is_none())
        .map_or(0, |pending| pending.challenge.charge_cents);
    Ok(
        json!({"provider":policy.settings.provider,"organization_id":policy.organization_id,
        "enabled":policy.enabled,"status":policy.status,"settings":policy.settings,
        "message":policy.message,"available_units":policy.balance_units,"next_check_at":policy.next_check_at,
        "spent_monthly_cents":spent,"reserved_cents":reserved,
        "remaining_monthly_cents":policy.settings.monthly_limit_cents.saturating_sub(spent).saturating_sub(reserved),
        "approval_url":policy.pending.as_ref().and_then(|pending|pending.approval_url.as_ref()),
        "wallet_action_required":policy.pending.as_ref().is_some_and(|pending|pending.wallet_status.as_deref()==Some("requires_action")),
        "pending":policy.pending.as_ref().map(|pending|json!({"id":pending.id,"phase":pending.phase,"charge_cents":pending.challenge.charge_cents,"submitted_at":pending.submitted_at})),
        "last_payment":policy.last_payment}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            provider: "tuara".into(),
            threshold_cents: 500,
            amount_cents: 2000,
            max_charge_cents: 2048,
            monthly_limit_cents: 4096,
            terms_version: "2026-09".into(),
            accept_terms: true,
        }
    }

    #[test]
    fn threshold_uses_exact_signed_units_and_requires_explicit_limits() {
        let base = settings();
        assert!(base.validate().is_ok());
        assert!(base.low(-1));
        assert!(base.low(499_999_999));
        assert!(!base.low(500_000_000));
        assert!(
            Settings {
                accept_terms: false,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                monthly_limit_cents: 2000,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                threshold_cents: u64::MAX,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                max_charge_cents: 50_001,
                ..base
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn utc_month_boundary_and_organization_identity_are_stable() {
        let august = time::Date::from_calendar_date(2026, time::Month::August, 31)
            .unwrap()
            .with_hms(23, 59, 59)
            .unwrap()
            .assume_utc()
            .unix_timestamp();
        assert!(month_start(august + 1).unwrap() > month_start(august).unwrap());
        let a = reqwest::Url::parse("https://tuara.com/router/v1").unwrap();
        let b = reqwest::Url::parse("https://tuara.com/another").unwrap();
        assert_eq!(identity(&a, "org-1"), identity(&b, "org-1"));
        assert_ne!(identity(&a, "org-1"), identity(&a, "org-2"));
    }
}
