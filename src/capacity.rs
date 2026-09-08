//! Account-wide observations are separate from per-attempt token accounting.
use crate::{
    config::{ExecutorConfig, Settings},
    management,
    store::{Store, now},
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub account: String,
    pub provider: String,
    pub window: String,
    pub used_percent: Option<f64>,
    pub reset_at: Option<i64>,
    pub observed_at: i64,
    pub source: String,
}
pub fn account(c: &ExecutorConfig) -> String {
    c.account.clone().unwrap_or_else(|| {
        if c.auth_mode == "api" {
            format!("{}:api:{}:{}", c.kind, c.base_url, c.api_key_env)
        } else {
            format!("{}:login", c.kind)
        }
    })
}
pub fn observe(db: &Store, s: &Snapshot) -> Result<()> {
    ensure!(
        !s.account.is_empty() && !s.provider.is_empty() && !s.window.is_empty(),
        "account, provider and window required"
    );
    ensure!(
        ["provider", "local_budget"].contains(&s.source.as_str()),
        "invalid capacity source"
    );
    ensure!(
        s.used_percent
            .is_none_or(|v| v.is_finite() && (0.0..=100.0).contains(&v)),
        "invalid usage percentage"
    );
    ensure!(
        s.observed_at <= now() + 60 && s.observed_at >= 0,
        "invalid observation time"
    );
    let policy = crate::fleet::load()?.capacity_policy;
    db.atomic(||{
        let old=db.rows("SELECT used,observed,reset FROM account_capacity WHERE account=? AND window=?",&[&s.account,&s.window])?;
        if old.first().is_some_and(|v|v["observed"].as_i64().unwrap_or(0)>s.observed_at){return Ok(());}
        db.conn.execute("INSERT INTO account_capacity VALUES(?,?,?,?,?,?,?) ON CONFLICT(account,window) DO UPDATE SET provider=excluded.provider,used=excluded.used,reset=excluded.reset,observed=excluded.observed,source=excluded.source",params![s.account,s.window,s.provider,s.used_percent,s.reset_at,s.observed_at,s.source])?;
        let level=|v:f64|if v>=100.0{3}else if v>=policy.switch_percent{2}else if v>=policy.warn_percent{1}else{0};
        let before=old.first().filter(|v|v["reset"].as_i64()==s.reset_at).and_then(|v|v["used"].as_f64()).map(level).unwrap_or(0);
        let after=s.used_percent.map(level).unwrap_or(0);
        if after>before {management::event(db,"account.capacity",json!({"snapshot":s,"level":after}))?;}
        Ok(())
    })
}
pub fn available(db: &Store, id: &str) -> Result<bool> {
    let policy = crate::fleet::load()?.capacity_policy;
    for row in db.rows("SELECT * FROM account_capacity WHERE account=?", &[&id])? {
        let reset = row["reset"].as_i64();
        if reset.is_some_and(|v| v <= now()) {
            continue;
        }
        // Observations expire after five minutes; a confirmed exhausted window remains held until reset.
        let used = row["used"].as_f64().unwrap_or(0.0);
        if row["observed"].as_i64().unwrap_or(0) + policy.stale_seconds < now()
            && !(used >= 100.0 && reset.is_some())
        {
            continue;
        }
        if used >= policy.switch_percent {
            return Ok(false);
        }
    }
    Ok(true)
}
pub fn select(db: &Store, settings: &Settings, role: &str) -> Result<Option<String>> {
    let mut current = role;
    let mut seen = std::collections::BTreeSet::new();
    while seen.insert(current) {
        let Some(c) = settings.executor(current) else {
            return Ok(Some(current.into()));
        };
        if available(db, &account(&c))? {
            return Ok(Some(current.into()));
        }
        let Some(next) = settings.fallbacks.get(current) else {
            return Ok(None);
        };
        current = next;
    }
    Ok(None)
}
pub fn report(db: &Store) -> Result<Value> {
    let policy = crate::fleet::load()?.capacity_policy;
    let settings = Settings::load_user()?;
    let mut accounts = serde_json::Map::new();
    for config in settings.resolved().values() {
        let id = account(config);
        accounts.entry(id.clone()).or_insert(
            json!({"account":id,"provider":config.kind,"capacity":"unknown","windows":[]}),
        );
    }
    for row in db.rows(
        "SELECT * FROM account_capacity ORDER BY account,window",
        &[],
    )? {
        let id = row["account"].as_str().unwrap_or_default();
        let entry = accounts
            .entry(id.to_owned())
            .or_insert(json!({"account":id,"provider":row["provider"],"windows":[]}));
        let stale = row["observed"].as_i64().unwrap_or(0) + policy.stale_seconds < now()
            || row["reset"].as_i64().is_some_and(|v| v <= now());
        let mut window = row.clone();
        window["stale"] = json!(stale);
        entry["windows"]
            .as_array_mut()
            .expect("windows array")
            .push(window);
        entry["capacity"] = json!(if stale { "unknown" } else { "reported" });
        entry["eligible"] = json!(available(db, id)?);
    }
    Ok(json!({"accounts":accounts.into_values().collect::<Vec<_>>()}))
}
/// Only explicit machine-readable quota windows count as account capacity.
pub fn ingest(db: &Store, c: &ExecutorConfig, event: &Value) -> Result<()> {
    let limits = event.get("rate_limits").or_else(|| event.get("rateLimits"));
    if let Some(limits) = limits {
        for window in ["primary", "secondary"] {
            let w = &limits[window];
            if let Some(used) = w["used_percent"]
                .as_f64()
                .or_else(|| w["usedPercent"].as_f64())
            {
                observe(
                    db,
                    &Snapshot {
                        account: account(c),
                        provider: c.kind.clone(),
                        window: window.into(),
                        used_percent: Some(used),
                        reset_at: w["resets_at"].as_i64().or_else(|| w["resetsAt"].as_i64()),
                        observed_at: now(),
                        source: "provider".into(),
                    },
                )?;
            }
        }
    }
    Ok(())
}

pub fn ingest_headers(
    db: &Store,
    c: &ExecutorConfig,
    headers: &reqwest::header::HeaderMap,
    status: u16,
) -> Result<()> {
    for prefix in ["x-ratelimit-", "anthropic-ratelimit-"] {
        for kind in ["requests", "tokens"] {
            let number = |suffix: &str| {
                headers
                    .get(format!("{prefix}{suffix}"))
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok())
            };
            let pair = if prefix == "x-ratelimit-" {
                (
                    number(&format!("limit-{kind}")),
                    number(&format!("remaining-{kind}")),
                )
            } else {
                (
                    number(&format!("{kind}-limit")),
                    number(&format!("{kind}-remaining")),
                )
            };
            if let (Some(limit), Some(remaining)) = pair
                && limit > 0.0
                && remaining >= 0.0
                && remaining <= limit
            {
                observe(
                    db,
                    &Snapshot {
                        account: account(c),
                        provider: c.kind.clone(),
                        window: format!("api-rate-{kind}"),
                        used_percent: Some(100.0 * (1.0 - remaining / limit)),
                        reset_at: None,
                        observed_at: now(),
                        source: "provider".into(),
                    },
                )?;
            }
        }
    }
    if status == 429
        && let Some(seconds) = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok())
    {
        observe(
            db,
            &Snapshot {
                account: account(c),
                provider: c.kind.clone(),
                window: "api-throttle".into(),
                used_percent: Some(100.0),
                reset_at: Some(now() + i64::from(seconds)),
                observed_at: now(),
                source: "provider".into(),
            },
        )?;
    }
    Ok(())
}

pub fn budgets(db: &Store) -> Result<()> {
    for b in crate::fleet::load()?.budgets {
        if b.reset_at <= now() {
            continue;
        }
        let mut tokens = 0u64;
        let mut unknown_tokens = false;
        let mut usd = 0.0;
        let mut unknown = false;
        for row in db.rows("SELECT a.usage FROM attempts a JOIN attempt_accounts c ON c.attempt=a.id WHERE c.account=? AND a.started>=? AND a.started<?",&[&b.account,&b.since,&b.reset_at])? {
            let value:Value=row["usage"].as_str().and_then(|s|serde_json::from_str(s).ok()).unwrap_or(Value::Null);
            let (t,c)=crate::metrics::totals(&value);if let Some(t)=t{tokens=tokens.saturating_add(t);}else{unknown_tokens=true;}
            if let Some(c)=c{usd+=c;}else{unknown=true;}
        }
        for (kind, used) in [
            (
                "tokens",
                b.tokens.and_then(|limit| {
                    if unknown_tokens && tokens < limit {
                        None
                    } else {
                        Some(100.0 * tokens as f64 / limit as f64)
                    }
                }),
            ),
            (
                "usd",
                b.usd.and_then(|limit| {
                    if unknown && usd < limit {
                        None
                    } else {
                        Some(100.0 * usd / limit)
                    }
                }),
            ),
        ] {
            if (kind == "tokens" && b.tokens.is_none()) || (kind == "usd" && b.usd.is_none()) {
                continue;
            }
            observe(
                db,
                &Snapshot {
                    account: b.account.clone(),
                    provider: "configured".into(),
                    window: format!("local-budget-{kind}-{}", b.since),
                    used_percent: used.map(|v| v.min(100.0)),
                    reset_at: Some(b.reset_at),
                    observed_at: now(),
                    source: "local_budget".into(),
                },
            )?;
        }
    }
    Ok(())
}

/// Read the installed Codex app-server account API; never starts a thread or model turn.
pub async fn codex_probe(db: &Store, config: &ExecutorConfig) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let program = config.program.as_deref().unwrap_or("codex");
    let mut command = tokio::process::Command::from(crate::executor::clean_command(program));
    command
        .args(["app-server", "--stdio"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("capacity probe stdin missing"))?;
    let mut output = tokio::io::BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("capacity probe stdout missing"))?,
    );
    let operation = async {
        for (id, method, params) in [
            (
                1,
                "initialize",
                json!({"clientInfo":{"name":"task","version":env!("CARGO_PKG_VERSION")}}),
            ),
            (2, "account/rateLimits/read", json!({})),
        ] {
            input
                .write_all(
                    format!("{}\n", json!({"id":id,"method":method,"params":params})).as_bytes(),
                )
                .await?;
            loop {
                let mut line = String::new();
                let n = (&mut output)
                    .take(128 * 1024 + 1)
                    .read_line(&mut line)
                    .await?;
                ensure!(
                    n > 0 && n <= 128 * 1024,
                    "capacity probe returned an invalid frame"
                );
                let response: Value = serde_json::from_str(&line)?;
                if response["id"] != id {
                    continue;
                }
                ensure!(
                    response["error"].is_null(),
                    "Codex account capacity API unavailable"
                );
                if id == 2 {
                    ingest(db, config, &response["result"])?;
                }
                break;
            }
            if id == 1 {
                input.write_all(b"{\"method\":\"initialized\"}\n").await?;
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), operation)
        .await
        .map_err(|_| anyhow::anyhow!("capacity probe timed out"))
        .and_then(|v| v);
    drop(input);
    let _ = child.start_kill();
    let _ = child.wait().await;
    result
}
pub async fn subscriptions(db: &Store) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    // Probe only accounts actually used here, using the pinned executable of that attempt.
    for row in db.rows("SELECT o.settings,c.role FROM attempt_accounts c JOIN attempts a ON a.id=c.attempt JOIN steps t ON t.id=a.step JOIN tasks o ON o.id=t.task WHERE a.started>? ORDER BY a.started DESC",&[&(now()-86400)])?{
        let settings:Settings=serde_json::from_str(row["settings"].as_str().ok_or_else(||anyhow::anyhow!("settings missing"))?)?;
        let Some(config)=row["role"].as_str().and_then(|r|settings.executor(r))else{continue};
        if config.kind=="codex"&&config.auth_mode=="login"&&seen.insert(account(&config)) && codex_probe(db,&config).await.is_err(){management::event(db,"account.refresh_failed",json!({"account":account(&config),"capacity":"unknown"}))?;}
    }
    Ok(())
}

pub fn select_for_step(
    db: &Store,
    settings: &Settings,
    role: &str,
    step: &str,
) -> Result<Option<String>> {
    let failures: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE step=? AND state='failed'",
        [step],
        |r| r.get(0),
    )?;
    let mut current = role;
    for _ in 0..failures {
        if let Some(next) = settings.fallbacks.get(current) {
            current = next;
        } else {
            break;
        }
    }
    select(db, settings, current)
}
