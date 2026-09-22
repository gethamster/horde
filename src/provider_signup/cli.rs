//! Human-facing provider funding commands. Payment authority stays in daemon RPCs.
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};

#[derive(Args)]
pub struct SignupArgs {
    /// Tuara preset or a configured Tuara provider name.
    #[arg(default_value = "tuara")]
    pub provider: String,
    /// Name for the new Tuara organization.
    #[arg(long)]
    pub organization: Option<String>,
    /// Agent name registered with Tuara (default: horde).
    #[arg(long)]
    pub agent: Option<String>,
    /// Initial credit in US dollars, for example 20 or 20.00.
    #[arg(long)]
    pub amount: Option<String>,
    /// Maximum total card charge in US dollars, including fees.
    #[arg(long)]
    pub max_charge: Option<String>,
    /// Exact Tuara terms version to accept; read https://tuara.com/terms/.
    #[arg(long)]
    pub terms_version: Option<String>,
    /// Accept the specified terms and authorize the stated funding and charge limit.
    #[arg(long)]
    pub accept_terms: bool,
    /// Authorize replacing an existing provider key after successful signup.
    #[arg(long)]
    pub replace_existing: bool,
    /// Resume this signup when used alone; otherwise use it for the new signup.
    #[arg(long)]
    pub request_id: Option<String>,
}

#[derive(Args, Default)]
pub struct TopupArgs {
    /// Tuara preset or a configured Tuara provider name.
    #[arg(default_value = "tuara")]
    pub provider: String,
    /// Show the saved top-up policy without making a payment request.
    #[arg(long, conflicts_with_all = ["disable", "check"])]
    pub status: bool,
    /// Stop automatic top-ups.
    #[arg(long, conflicts_with = "check")]
    pub disable: bool,
    /// Check the balance and advance an authorized top-up if needed.
    #[arg(long)]
    pub check: bool,
    /// Top up below this balance, in US dollars.
    #[arg(long)]
    pub threshold: Option<String>,
    /// Credit per top-up, in US dollars.
    #[arg(long)]
    pub amount: Option<String>,
    /// Maximum total card charge per top-up, including fees, in US dollars.
    #[arg(long)]
    pub max_charge: Option<String>,
    /// Maximum total recurring charges per month, including fees, in US dollars.
    #[arg(long)]
    pub monthly_limit: Option<String>,
    /// Exact Tuara terms version to accept; read https://tuara.com/terms/.
    #[arg(long)]
    pub terms_version: Option<String>,
    /// Accept the specified terms and authorize recurring charges within these limits.
    #[arg(long)]
    pub accept_terms: bool,
}

fn dollars(text: &str) -> Result<u64> {
    let text = text.trim();
    let mut parts = text.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && parts.next().is_none(),
        "enter a dollar amount such as 20 or 20.48, with at most two decimal places"
    );
    let fraction = fraction.unwrap_or("");
    ensure!(
        fraction.len() <= 2 && fraction.bytes().all(|byte| byte.is_ascii_digit()),
        "dollar amounts must have at most two decimal places"
    );
    let cents = whole
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(100));
    let remainder = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>()? * 10,
        _ => fraction.parse::<u64>()?,
    };
    cents
        .and_then(|value| value.checked_add(remainder))
        .context("dollar amount is too large")
}

fn money(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn ask(label: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    ensure!(
        std::io::stdin().read_line(&mut line)? > 0,
        "input ended before setup was authorized"
    );
    let answer = line.trim();
    Ok(if answer.is_empty() {
        default.to_owned()
    } else {
        answer.to_owned()
    })
}

fn confirm(label: &str) -> Result<bool> {
    Ok(matches!(
        ask(&format!("{label} (y/N)"), "n")?.to_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn required(value: Option<String>, prompt: &str, default: &str, terminal: bool) -> Result<String> {
    let value = match value {
        Some(value) => value,
        None if terminal => ask(prompt, default)?,
        None => bail!(
            "missing funding option; run this command in a terminal for guided setup or supply its required flags"
        ),
    };
    ensure!(!value.trim().is_empty(), "{prompt} cannot be empty");
    Ok(value)
}

type Call<'a> = dyn FnMut(&str, Value) -> Result<Value> + 'a;

pub fn signup(
    options: SignupArgs,
    mut call: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<()> {
    let terminal = interactive();
    let resume = options.request_id.is_some()
        && options.organization.is_none()
        && options.amount.is_none()
        && options.max_charge.is_none()
        && options.terms_version.is_none()
        && !options.accept_terms
        && !options.replace_existing
        && options.agent.is_none();
    let request_id = options
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let state = if resume {
        println!("Signup reference: {request_id}");
        call(
            "provider_signup",
            json!({"action":"status","request_id":request_id}),
        )?
    } else {
        signup_start(&options, &request_id, terminal, &mut call)?
    };
    let provider = state["provider"]
        .as_str()
        .unwrap_or(&options.provider)
        .to_owned();
    let complete = advance_signup(&provider, &request_id, state, &mut call)?;
    if complete && terminal && confirm("Set up automatic Tuara top-ups now?")? {
        topup(
            TopupArgs {
                provider,
                ..Default::default()
            },
            call,
        )?;
    }
    Ok(())
}

fn signup_start(
    options: &SignupArgs,
    id: &str,
    terminal: bool,
    call: &mut Call<'_>,
) -> Result<Value> {
    ensure!(
        terminal
            || options.organization.is_some()
                && options.amount.is_some()
                && options.max_charge.is_some()
                && options.terms_version.is_some()
                && options.accept_terms,
        "noninteractive signup requires --organization, --amount, --max-charge, --terms-version and --accept-terms; run in a terminal for guided setup"
    );
    let explicit = options.accept_terms
        && options.amount.is_some()
        && options.max_charge.is_some()
        && options.terms_version.is_some();
    let organization = required(
        options.organization.clone(),
        "Organization name",
        "",
        terminal,
    )?;
    let amount = dollars(&required(
        options.amount.clone(),
        "Initial credit in dollars",
        "20.00",
        terminal,
    )?)?;
    let cap = default_cap(amount)?;
    let maximum = dollars(&required(
        options.max_charge.clone(),
        "Maximum total charge in dollars, including fees",
        &format!("{}.{:02}", cap / 100, cap % 100),
        terminal,
    )?)?;
    validate_charge(amount, maximum)?;
    let terms = required(
        options.terms_version.clone(),
        "Tuara terms version",
        "2026-09",
        terminal,
    )?;
    if !explicit {
        println!("Read the Tuara terms: https://tuara.com/terms/");
        ensure!(
            confirm(&format!(
                "Accept terms {terms} and fund {} with a total charge up to {}?",
                money(amount),
                money(maximum)
            ))?,
            "Signup cancelled before any account or payment request"
        );
    }
    println!("Signup reference: {id}");
    call(
        "provider_signup",
        json!({"action":"start","request_id":id,"provider":options.provider,"organization_name":organization,"agent_name":options.agent.as_deref().unwrap_or("horde"),"amount_cents":amount,"max_charge_cents":maximum,"terms_version":terms,"accept_terms":true,"replace_existing":options.replace_existing}),
    )
}

fn default_cap(amount: u64) -> Result<u64> {
    amount
        .checked_mul(1024)
        .and_then(|value| value.checked_add(999))
        .map(|value| value / 1000)
        .context("dollar amount is too large")
}

fn validate_charge(amount: u64, maximum: u64) -> Result<()> {
    ensure!(
        amount >= 500 && maximum >= amount && maximum <= 50_000,
        "credit must be at least $5 and the total charge ceiling must cover it without exceeding Link's $500 limit"
    );
    Ok(())
}

fn advance_signup(provider: &str, id: &str, mut state: Value, call: &mut Call<'_>) -> Result<bool> {
    for step in 0..5 {
        let status = state["status"]
            .as_str()
            .context("signup returned no status")?
            .to_owned();
        match status.as_str() {
            "succeeded" => {
                println!(
                    "Your Tuara account is ready. Its verified key is saved for the next Horde invocation."
                );
                return Ok(true);
            }
            "uncertain" | "submitting" => bail!(
                "Reconcile this payment with Tuara and Link before continuing. Horde will not charge again. Signup reference: {id}"
            ),
            "failed" | "expired" | "cancelled" => bail!(
                "Tuara signup {status}. Inspect this signup before starting another payment. Reference: {id}"
            ),
            "preparing" | "awaiting_wallet" | "awaiting_approval" | "credential_received" => (),
            _ => bail!(
                "Tuara signup returned an unsupported status; retain reference {id} and inspect it before retrying"
            ),
        }
        if state["wallet_action_required"] == true {
            println!(
                "Link needs you to resolve an action in your wallet before signup can continue."
            );
            if step > 0 {
                break;
            }
        }
        if let Some(url) = state["approval_url"].as_str() {
            println!("Approve the payment in Link: {url}");
        }
        let next = call(
            "provider_signup",
            json!({"action":"resume","request_id":id}),
        )?;
        if next["status"] == state["status"] {
            if next["wallet_action_required"] == true {
                println!("Resolve the action in Link, then run the continuation command below.");
            } else {
                match status.as_str() {
                    "credential_received" => println!(
                        "The signup response is saved; key verification or installation still needs to finish. No further payment is needed."
                    ),
                    "awaiting_approval" => println!("Signup is waiting for Link approval."),
                    "awaiting_wallet" => println!(
                        "Signup is waiting for the Link wallet. Install link-cli and sign in with `link-cli auth login` if needed."
                    ),
                    _ => println!("Signup has not advanced yet."),
                }
            }
            if let Some(message) = next["message"].as_str() {
                println!("{message}");
            }
            if let Some(url) = next["approval_url"].as_str() {
                println!("Approve the payment in Link: {url}");
            }
            state = next;
            break;
        }
        state = next;
    }
    match state["status"].as_str() {
        Some("succeeded") => {
            println!(
                "Your Tuara account is ready. Its verified key is saved for the next Horde invocation."
            );
            return Ok(true);
        }
        Some("uncertain" | "submitting") => bail!(
            "Reconcile this payment with Tuara and Link before continuing. Horde will not charge again. Signup reference: {id}"
        ),
        Some("failed" | "expired" | "cancelled") => {
            bail!("Tuara signup stopped. Inspect reference {id} before starting another payment.")
        }
        _ => (),
    }
    println!(
        "Continue with: {} config provider signup {} --request-id {}",
        crate::branding::cli_name(),
        shell(provider),
        shell(id)
    );
    Ok(false)
}

fn shell(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub fn topup(options: TopupArgs, mut call: impl FnMut(&str, Value) -> Result<Value>) -> Result<()> {
    let action = if options.status {
        "status"
    } else if options.disable {
        "disable"
    } else if options.check {
        "check"
    } else {
        "configure"
    };
    let args = if action == "configure" {
        topup_settings(&options, interactive())?
    } else {
        ensure!(
            options.threshold.is_none()
                && options.amount.is_none()
                && options.max_charge.is_none()
                && options.monthly_limit.is_none()
                && options.terms_version.is_none()
                && !options.accept_terms,
            "use funding settings when configuring top-ups, without --status, --disable or --check"
        );
        json!({"action":action,"provider":options.provider})
    };
    let result = call("provider_topup", args)?;
    print_topup(&result, action);
    Ok(())
}

fn topup_settings(options: &TopupArgs, terminal: bool) -> Result<Value> {
    ensure!(
        terminal
            || options.threshold.is_some()
                && options.amount.is_some()
                && options.max_charge.is_some()
                && options.monthly_limit.is_some()
                && options.terms_version.is_some()
                && options.accept_terms,
        "noninteractive top-up setup requires --threshold, --amount, --max-charge, --monthly-limit, --terms-version and --accept-terms; run in a terminal for guided setup"
    );
    let explicit = options.accept_terms
        && options.threshold.is_some()
        && options.amount.is_some()
        && options.max_charge.is_some()
        && options.monthly_limit.is_some()
        && options.terms_version.is_some();
    let threshold = dollars(&required(
        options.threshold.clone(),
        "Top up when the balance falls below, in dollars",
        "5.00",
        terminal,
    )?)?;
    let amount = dollars(&required(
        options.amount.clone(),
        "Credit per top-up in dollars",
        "20.00",
        terminal,
    )?)?;
    let cap = default_cap(amount)?;
    let maximum = dollars(&required(
        options.max_charge.clone(),
        "Maximum total charge per top-up in dollars, including fees",
        &format!("{}.{:02}", cap / 100, cap % 100),
        terminal,
    )?)?;
    let monthly = dollars(&required(
        options.monthly_limit.clone(),
        "Maximum recurring charges per UTC calendar month in dollars, including fees",
        "100.00",
        terminal,
    )?)?;
    let terms = required(
        options.terms_version.clone(),
        "Tuara terms version",
        "2026-09",
        terminal,
    )?;
    validate_charge(amount, maximum)?;
    ensure!(
        (1..=50_000).contains(&threshold) && (maximum..=100_000_000).contains(&monthly),
        "the threshold must be between $0.01 and $500; the monthly limit must cover one authorized charge and cannot exceed $1,000,000"
    );
    if !explicit {
        println!("Read the Tuara terms: https://tuara.com/terms/");
        ensure!(
            confirm(&format!(
                "Accept terms {terms} and authorize recurring {} top-ups below {}, up to {} per charge and {} per UTC calendar month including fees?",
                money(amount),
                money(threshold),
                money(maximum),
                money(monthly)
            ))?,
            "Automatic top-up setup cancelled without changing your policy"
        );
    }
    Ok(
        json!({"action":"configure","provider":options.provider,"threshold_cents":threshold,"amount_cents":amount,"max_charge_cents":maximum,"monthly_limit_cents":monthly,"terms_version":terms,"accept_terms":true}),
    )
}

fn print_topup(result: &Value, action: &str) {
    let enabled = result["enabled"].as_bool().unwrap_or(false);
    if action == "disable" || !enabled {
        println!("Automatic Tuara top-ups are disabled.");
    } else {
        println!("Automatic Tuara top-ups are enabled.");
    }
    if let Some(settings) = result.get("settings") {
        for (field, label) in [
            ("threshold_cents", "Balance threshold"),
            ("amount_cents", "Credit per top-up"),
            ("max_charge_cents", "Maximum total per charge"),
            ("monthly_limit_cents", "Monthly charge limit"),
        ] {
            if let Some(cents) = settings[field].as_u64() {
                println!("{label}: {}", money(cents));
            }
        }
    }
    if let Some(cents) = result["remaining_monthly_cents"].as_u64() {
        println!("Remaining monthly allowance: {}", money(cents));
    }
    if result["wallet_action_required"] == true {
        println!("Resolve the requested action in Link before the next top-up can proceed.");
    }
    if let Some(url) = result["approval_url"].as_str() {
        println!("Approve the top-up in Link: {url}");
    }
    match result["status"].as_str() {
        Some("budget_exhausted") => println!(
            "The remaining monthly allowance cannot cover another top-up. No additional payment will be submitted within this budget."
        ),
        Some("needs_attention") => println!(
            "Automatic top-ups need attention. Check the provider account and Link wallet before continuing."
        ),
        Some("payment_received") => {
            println!("A top-up response was received. Horde is checking the result.")
        }
        Some("awaiting_wallet") => {
            println!("The top-up quote is ready; Horde will request wallet authorization next.")
        }
        Some("awaiting_approval") => println!("The next top-up is waiting for Link approval."),
        Some("uncertain" | "submitting") => println!(
            "Payment outcome is unknown. Reconcile it with Tuara and Link; Horde will not charge again."
        ),
        Some("watching") => {
            println!("Horde is watching the balance and will stay within your authorized limits.")
        }
        _ => (),
    }
    if let Some(message) = result["message"].as_str() {
        println!("{message}");
    }
}
