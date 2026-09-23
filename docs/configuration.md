# Configuration

Defaults are autonomous execution, four concurrent workers, Tuara over an API key for planning/implementation/review, and no delivery. `codex` and `claude` roles are configured for those CLIs under subscription login. Model names for the harnesses inherit their upstream defaults unless explicitly set.

Installing runs `horde config init`, which writes a starter `config.toml` and a private `credentials.env` into the configuration directory. It creates neither if it is already there, so it is safe to run again. `horde config` prints the complete default TOML.

## Set up through any MCP client

An MCP client connected to Horde's unbound administrative `horde mcp`
connection is the parent agent. The connection does not choose the model for
child workers. Ask that agent to set up Horde in ordinary language; it starts
with `agent_setup` action `inspect`, which reports the current parent connection,
child executor roles, provider authentication, controller state, and Tuara
wallet readiness. With an absolute repository path, it also reports whether
that checkout is registered and offers `project_repo_add` when needed. You do
not need to provide JSON.

The agent can call `agent_setup` action `configure_workers` with a configured
provider and role names such as `planner`, `worker`, and `reviewer`. It assigns
those child roles without requiring or changing the provider credential. To add
or authenticate a provider, the agent uses `configure_provider` or
`provider_login`. For a new Tuara account it uses `provider_wallet` to install
and connect Link, `provider_signup` to create the funded account, and
`provider_topup` to enable recurring funding within the limits you authorize.
Signup and recurring funding require separate choices and consent; the agent
collects them in conversation and supplies the tool arguments itself.

Remote fleet workers use `configure_controller`, `create_fleet_key`, and
`join_worker`. The fleet credential stays in a private file or the target
platform's secret store. Horde reports the remaining platform actions when
the MCP connection cannot perform them. An MCP client must first be connected
to Horde: local clients can launch `horde mcp` over stdio, while remote clients
need a reachable, authenticated transport. The Horde installer does not
register a connector in every third-party client. Codex and Claude users can
also run `horde init` to install repository instructions and client-specific MCP
configuration.

## Decision models and advisory routing

Horde can record decision-model routing advice without changing which executor runs a step. The feature is disabled by default and can be enabled only in operator-owned configuration. A repository `.horde.toml` cannot enable or alter it. Fresh decision settings name Tuara as the provider but leave the endpoint and credential source empty. This example uses the Tuara Jev catalog ID validated by the [live smoke test](decision-routing-v1-report.md):

```toml
[decision]
mode = "shadow"
backend = "tuara"
base_url = "https://tuara.com/router"
api_key_env = "TUARA_API_KEY"
model = "XXXXTSJV130XXX"
protocol = "systemone-v1"
policy = "routing-v1"
deadline_ms = 30000
max_attempts = 2
max_decisions_per_task = 64

[[decision.capability_guidance]]
runtime = "local"
capability = "worker"
description = "Use the configured worker for repository changes and their verification."
```

Horde appends `/v1/systemone` to this decision base URL. Generative providers use
their separate `/router/v1` base URL. To use TypeSafe directly, set
`backend = "typesafe"`, `base_url = "https://api.typesafe.ai"`,
`api_key_env = "TYPESAFE_API_KEY"`, and `model = "jev-1.13.0"`.
Add `review_enabled = true` inside `[decision]` for advisory work-product reviews.
The guidance capability must match the actual role or execution-policy binding.

Put the key named by `api_key_env` in the private `credentials.env` file or the daemon environment. Horde snapshots only the variable name. Before every request, the daemon confirms that the current user configuration still authorizes the task's pinned provider, endpoint, protocol, model, policy, and limits. A custom decision endpoint requires an explicit `backend` and `api_key_env`; Horde never sends the TypeSafe key to one implicitly. Legacy mode-only enabled settings retain the TypeSafe defaults.

The daemon evaluates only fresh, available capabilities that the task's execution policy already permits. Without an execution policy, it considers the conventional local role and its configured fallback chain. Every candidate needs operator guidance; missing guidance produces an abstention. The decision model never receives tools or worker credentials, and shadow output cannot change the selected executor. A different provider or model cannot inherit active browser/context behavior or delivery authority.

Use `horde decisions TASK_ID` to inspect the durable records. `--after` and `--limit` page through them, and `horde metrics TASK_ID` reports decision requests, retries, latency, token usage, abstentions, and agreement with the baseline. Provider cost remains unknown unless a later backend reports it.

Automatic merge and deployment have a separate operator-only `[automatic_delivery]` policy. It is disabled by default and cannot become operational until a held-out delivery qualification has been evaluated and enrolled. See [GitHub delivery](delivery.md#guarded-automatic-delivery) for the scope, evidence contract, and current limit.

Add a provider and its key without editing either file:

```sh
horde config provider add          # pick a provider, a model, paste the key
horde config provider list         # what is configured, and whether each key reads
horde config models default        # what that endpoint accepts as a model
```

Presets are `tuara`, `codex`, `claude`, `openai`, and `anthropic`; anything else is described with `--kind`, `--base-url`, and `--api-key-env`. For scripts, `printf '%s\n' "$KEY" | horde config provider add tuara --key-stdin --use-for planner,worker,reviewer`. The key is never taken as a command-line argument, and lands only in `credentials.env`.

For the `default` project, settings merge in this order:

1. Built-in defaults.
2. `$XDG_CONFIG_HOME/horde/config.toml`, or `~/.config/horde/config.toml`.
3. `.horde.toml` in the submitted repository (legacy path).
4. `.horde/horde.toml` in the submitted repository (preferred path).

Later files override matching settings and retain settings they do not restate.
Both repository paths remain supported; if both exist, `.horde/horde.toml` wins.
To migrate, move `.horde.toml` to `.horde/horde.toml` after updating Horde.

The merged settings are pinned to the task. Later file changes affect new tasks.
Credentials are read at invocation time, so replacing a key or CLI login can serve
existing tasks that use that provider. Running invocations retain their credentials.

## Add or change an account through your agent

You can ask your connected agent to replace an API key or sign in to another
Codex, Claude, or Tuara account without terminal access. These are administrative tools;
a task-scoped worker token cannot call them. The connection to Horde must remain
available, but these operations do not make model requests or require model quota.

For an API key, the agent calls `agent_setup` with a request such as:

```json
{"action":"configure_provider","provider":"openai","credential":"YOUR_NEW_API_KEY"}
```

Give the key to your trusted agent through your chosen channel. `credential`
accepts the value directly; `credential_env` and `credential_file` remain available
when the agent already has a reference. Supply only one source. Horde writes the
key to its private `credentials.env`, preserves other accounts and settings, and
omits the key from its response. `roles` optionally selects which roles use the
provider for new tasks. Omit it when rotating an existing provider's key.

The response reports `credential_activation: "next_invocation"` and
`restart_required: false`. An explicitly supplied credential takes precedence
over an older value inherited by the daemon, so the replacement needs no restart.
Horde records that variable's file priority in private `credential-overrides.json`
beside the credentials file; this record contains variable names only. Other
variables keep their existing environment-first lookup. Saving a key does not
verify access; `provider_api` remains `not_probed`.

For Tuara, ask your connected agent to guide you through obtaining and verifying
a key. The agent uses the account-setup operation internally and directs you to
[Tuara's key page](https://tuara.com/app/buy/keys). Sign in, create or copy an
inference API key, and give it to your agent. The agent submits it as `input` with
the returned `session_id`. Horde checks Tuara's account introspection endpoint
for an API key with `router:invoke` access before saving it. Failed verification,
cancellation, or expiry leaves the existing key intact. Success reports
`provider_authentication: "verified"` and `credential_activation: "next_invocation"`;
it does not prove available quota. This requires no Tuara CLI or inference request.
Tuara's account OAuth grants do not include inference access.

For a new funded Tuara account, use [automatic signup](#create-a-funded-tuara-account)
below. The key-page flow remains available for an account you already have.

You can also name an existing Tuara provider. The `tuara` preset reuses a matching
configured provider, usually `default`, and preserves its model and role settings.

For a subscription account, the agent starts a `provider_login` session:

```json
{"action":"start","provider":"codex","request_id":"connect-codex-1","timeout_seconds":600}
```

Use `claude` for Claude Code. Horde runs the installed CLI's login command and
returns a `session_id`. The agent polls `status` with that ID and relays the CLI's
URL and device code, or its request for a manual authorization code. Open the
provider's URL in your browser and complete sign-in. If the CLI asks for a code,
give it to the agent, which submits it through the same session:

```json
{"action":"submit","session_id":"SESSION_ID","input":"AUTHORIZATION_CODE"}
```

The agent then polls `status` until the session finishes, or calls `cancel` if
you abandon sign-in. Horde reports `provider_authentication: "verified"` only
after both login and the CLI's authentication-status check succeed. Available
quota remains `unknown`. Horde uses the provider's login pages and does not host
a separate connection form.

A session survives tool calls and client disconnects until its deadline, which
defaults to 600 seconds and can be set from 1 through 1800 seconds. Retrying
`start` with the same request ID and arguments returns that session. Horde retains
this deduplication record through the deadline plus one hour while the daemon
lives. A daemon restart ends the session; start a new login afterward.

Codex and Claude each use their shared CLI login store on that runtime, so signing
in replaces the account used by other invocations of that CLI. This flow does not
create separate subscription profiles. Existing tasks keep their saved provider
selection, and changing role settings affects new tasks. After an effective API
key change or a verified login, Horde discards affected provider quota observations
and treats capacity as unknown; local budget observations remain. Account changes
do not reconcile or automatically resume uncertain work.

## Create a funded Tuara account

Ask your connected agent to create a Tuara account, or run the guided command:

```sh
horde config provider signup tuara
```

Horde asks for an organization name, an initial credit amount, and a maximum
charge including fees. It shows the [Tuara terms](https://tuara.com/terms/) and
asks you to accept a specific version before starting. The walkthrough proposes
$20 credit and a $20.48 maximum charge; you can change both. It defaults to
refusing payment until you consent. Your agent can gather the same choices in
ordinary language and call Horde internally; you do not need to write JSON.

Your connected agent prepares the private Link wallet connection before it starts
signup. It first calls `provider_wallet` with `action: "inspect"`. If Link is
missing or not ready, it calls `install` with a stable request ID and checks
`status` until the supervised installation finishes. It then calls
`login_start`, relays Link's verification URL and device phrase, and checks
`status` with the returned session ID until Link confirms the connection. Use
`cancel` with that session ID if you abandon sign-in. The agent uses `details`
to confirm safe wallet readiness before it starts a paid flow.

Link keeps payment methods in its hosted wallet at
[app.link.com/wallet](https://app.link.com/wallet). If `details` reports a
missing payment method or verification requirement, open the hosted wallet
and complete the action there. Link's agent wallet currently supports US
accounts. Horde's MCP tools never accept, display, or
store a card number, security code, or other full card details. When the
supervised installation completes, Horde uses its pinned Link CLI below its
configuration directory. Installing Link requires Node.js and npm on the host;
`provider_wallet inspect` reports when either is missing.
A Link account you already use with Grok Bot can be reused. If Grok Bot already
has a Link MCP connection, configure and use that connection separately; Horde
does not add or configure it.

After Link is ready, Horde creates the Tuara organization through MPP, captures
its new inference key privately, verifies it, and saves it for the next
invocation. You never need to copy the new key. Model capacity remains unknown.

The command advances signup through bounded steps and prints a reference if
wallet setup or approval is still needed. Continue the same signup with:

```sh
horde config provider signup tuara --request-id YOUR_SIGNUP_REFERENCE
```

For scripts, supply the choices explicitly. Dollar amounts accept at most two
decimal places. Missing consent or required choices in a noninteractive session
produce an actionable error instead of a prompt:

```sh
horde config provider signup tuara \
  --organization "My Agent Co" --agent horde \
  --amount 20 --max-charge 20.48 \
  --terms-version 2026-09 --accept-terms
```

When Tuara is running in Stripe test mode, add `--test-mode` (or set
`test_mode: true` in `provider_signup` over MCP). Horde then asks Link for a
test credential; Link does not charge the underlying payment method. The
test-mode choice is saved with the signup request so a later `resume` uses the
same mode. Confirm the account is in test mode before using this option; a
test credential will not fund a live account. You use your regular Link account
for device approval; there is no separate Link test account, and Horde never
asks for a test card number through MCP.

The minimum credit is $5, and Link limits the total charge to $500 including
fees. An existing provider key blocks signup unless you authorize
`--replace-existing`. Horde preserves model settings and role assignments.
After successful interactive signup, the walkthrough offers automatic top-up
setup; accepting signup alone does not enable recurring charges.

Signup requires an unbound default-project administrative connection. Worker
and project-scoped connections cannot call it. Private receipts in
`provider-signups/` beside `config.toml` survive daemon restarts. Reuse the printed
reference after a lost reply. A saved signup response allows verification and
key installation to resume without another payment. If the payment outcome is
unknown, Horde stops and requires reconciliation with Tuara and Link; it never
replays that payment or creates another account to recover.

Agents use `provider_signup` with `start`, `status`, `resume`, and `cancel`.
`start` validates a quote without paying. `status` reads local progress without
provider calls. Bounded resumes request approval for the individual payment, submit one approved
payment, and verify and save its key. At `credential_received`, another resume
finishes installation. At `wallet_action_required`, resolve the action in Link
before resuming. Cancellation is limited to before paid submission. See the
[operation reference](../skills/horde/references/operations.md#provider-account-setup)
for fields and states.

## Automatic Tuara top-ups

Ask your agent to keep a Tuara account funded within your chosen limits, or run:

```sh
horde config provider topup tuara
```

The walkthrough asks for the balance threshold, credit per top-up, maximum total
per charge, and monthly spending limit including fees for a UTC calendar month.
It then asks you to accept the terms and authorize recurring charges within those limits. It does
not enable a policy until you agree. Scripts can supply the choices directly:

```sh
horde config provider topup tuara \
  --threshold 5 --amount 20 --max-charge 20.48 --monthly-limit 100 \
  --terms-version 2026-09 --accept-terms
```

For a test-mode Tuara account, add `--test-mode` to the top-up policy too (or
set `test_mode: true` in `provider_topup` over MCP). It is saved with the
policy and used for every payment under that policy.

Horde checks the balance every 60 seconds and advances one funding step per
check. After a successful top-up it waits at least five minutes before another.
Link may require approval for each individual payment; an action that needs your attention
pauses progress until you resolve it in the wallet. Inspect, advance, or disable
the policy without editing configuration:

```sh
horde config provider topup tuara --status
horde config provider topup tuara --check
horde config provider topup tuara --disable
```

`--status` reads saved progress without a payment request. `--check` checks the
balance and may advance an already authorized payment; the daemon performs those
checks automatically while the policy is enabled. Disabling clears unpaid
pending work and stops future top-ups. It cannot reverse a submitted payment.

The monthly allowance counts total charges including fees by the UTC month of
paid submission. Disabling or changing a policy preserves its payment history.
Provider aliases for the same Tuara origin and organization share that budget
within one Horde configuration directory. The limit does not aggregate separate
Horde installations or spending outside this policy. An uncertain paid outcome
blocks repayment and needs Tuara and wallet reconciliation.

Signup and top-ups are tested with mock services and wallet processes; no paid
live funding flow has been validated. See [Tuara's MPP contract](https://tuara.com/docs/agents/signup/index.md)
for its signup and top-up behavior.

## Provider and executor settings

Configure skill directories under `[skills]` and select their names with a step’s `skills` field. Horde exposes selected names, pinned hashes, and resource locations in the worker prompt. The harness reads instructions and references progressively; child tasks receive the same pinned bundles. See [runtime skills](runtime-skills.md) for configuration, worker tools, and remote delivery.

```toml
concurrency = 4
autonomy = true
timeout_seconds = 1800
allow_commands = true

# A provider is a named endpoint and credential, written once.
[providers.default]
kind = "tuara"
auth_mode = "api"
base_url = "https://tuara.com/router/v1"
api_key_env = "TUARA_API_KEY"
model = "qwen/qwen3.8-27b"
max_tokens = 8192
max_price = "1.00" # Optional ceiling in dollars per million tokens, not a total budget.

# A role picks a provider and a model. Both are optional: no provider means
# providers.default, and no model means that provider's.
[executors.worker]

[executors.reviewer]
provider = "claude"
```

The key itself belongs in the **daemon** environment before starting it, or in a private mode-0600 `credentials.env` beside `config.toml`; only the name of the variable goes in `config.toml`. Worker command environments use an allowlist and omit provider keys. Codex and Claude use their installed CLI and existing credential store under `auth_mode = "login"`. For API-backed harnesses, set `auth_mode = "api"`, `base_url`, and `api_key_env` on the provider. A per-invocation loopback broker keeps the real key in the daemon and gives the harness a temporary credential limited to its model API. Real keys are never injected into harness child environments. Unsupported total-spend caps fail explicitly rather than being ignored. Claude's CLI budget option is passed through when configured.

`kind`, `auth_mode`, `base_url`, and `api_key_env` belong to a provider and cannot be restated on a role, so no role can pair one provider's harness with another's key. Nothing is inherited between providers either: one that omits `base_url` or `api_key_env` while it needs one is rejected at load.

For a local OpenAI-compatible server, the default provider needs only:

```toml
[providers.default]
base_url = "http://127.0.0.1:8122/v1"
api_key_env = "LOCAL_MODEL_KEY"
model = "auto"
```

Set `LOCAL_MODEL_KEY` in the daemon environment or its private credential file.
`auto` selects the only model in `/models`; an ambiguous catalog produces an error
listing its IDs. `horde doctor --provider default` prints the resolved model without
requesting a completion. Add `--probe` to test streamed tool calls.

Native providers accept `extra_body` request options. A role's `extra_body`
overrides matching provider keys; nested objects are replaced as a whole.
Set `stream = true` on the provider for first-token and tool-intent events.
`horde events TASK_ID` includes redacted tool arguments and results, limited to
512 bytes per field by default; the top-level `tool_event_bytes` setting controls
the limit. See the [native provider contract](native-providers.md) for examples,
reserved fields, stable request history, and loop detection.

For example, an API-backed Claude provider shared by two roles:

```toml
[providers.anthropic]
kind = "claude"
auth_mode = "api"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"

[executors.worker]
provider = "anthropic"
model = "your-explicit-model-id"

[executors.reviewer]
provider = "anthropic"
model = "a-stronger-model-id"
```

For Codex API authentication use `kind = "codex"`, `base_url = "https://api.openai.com/v1"`, and `api_key_env = "OPENAI_API_KEY"` on a provider. The shipped `codex` and `claude` providers use subscription login. The broker passes through SSE and provider errors and rejects requests outside the invocation's model endpoints.

The `claude` harness passes `--json-schema` so Claude Code validates the step's completion object (`result`, `accepted`, `artifacts`, and any declared named outputs) and returns it as structured output. If a reply still lacks that object, Horde resumes the same Claude session once and asks for only the JSON object. A second malformed reply fails the attempt, and both turns' usage is recorded. This needs a Claude Code CLI with `--json-schema`; 2.1.280 has it.

The shipped `grok` provider runs the Grok CLI (`grok -p PROMPT --output-format json --always-approve`, `kind = "grok"`) under its installed subscription login; `auth_mode = "api"` is refused for it. The harness writes the coordination MCP server into the workspace's `.grok/config.toml` (the shape `grok mcp add --scope project` writes), passes the prompt as a command-line argument (limit 200 KiB), and reads the single JSON reply. Use it as a fallback hop, for example `[fallbacks] codex = "grok"`.

Set `autonomy = false` to hold new tasks until the initial question is answered:

```sh
horde answer TASK_ID QUESTION_ID yes
```

### Network access for Codex executors

The Codex harness runs `codex exec --sandbox workspace-write`, and that sandbox blocks network access by default. A Codex step then cannot fetch dependencies; for example, `cargo check` in a fresh workspace fails because `index.crates.io` does not resolve. Set `network = true` on a provider, or on a single role, to allow network access inside the sandbox:

```toml
[executors.reviewer]
provider = "codex"
network = true          # passes -c sandbox_workspace_write.network_access=true
```

A role's `network` overrides its provider's, so `network = false` on a role keeps that role offline even when its provider allows network access. The default is off. Managed Codex accounts get the same setting on the app-server thread. File writes stay limited to the workspace; only the network rule changes. The setting affects only `kind = "codex"`: Claude Code's Bash tool and native command steps already have network access, and Grok does not use this sandbox.

Enabling `network` changes the role's configuration fingerprint, so execution pins that name the old configuration have to be renewed. A runtime older than this release rejects settings that contain `network`, instead of running the step without it.

## Separate projects and provider accounts

Create a project for each independent body of work, then register its checkouts
and approve the runtimes allowed to execute it:

```sh
horde project create horde --concurrency 4
horde project create hamster --concurrency 2
horde project repo-add horde /absolute/path/to/horde
horde project repo-add hamster /absolute/path/to/hamster
horde project runtime-grant horde local
horde project runtime-grant hamster local
horde --project horde submit "Implement the next change" --repo /absolute/path/to/horde
horde --project hamster list
horde list --all-projects
```

Projects have immutable IDs and human-readable slugs. A checkout and all of its
Git worktrees belong to one project. Separate clones of the same upstream may
belong to different projects. Registered repositories supply the project when
selection is unambiguous; pass `--project` when it cannot be inferred. A new
project starts with no account or runtime grants. The `default` project retains
legacy settings and runtime eligibility, subject to explicit revocation.

Configure each new project's providers in an administrator-owned TOML file:

```sh
horde project configure horde --file /private/path/horde.toml
horde project inspect horde
horde project update horde --concurrency 3
```

Horde validates the file before atomically replacing
`DATA_DIR/projects/PROJECT_ID/config.toml`, with mode 0600. Its settings merge
with the built-in defaults and then the registered repository's two configuration
files. New projects do not inherit the default project's user configuration.
Continue using `horde config` for the `default` project.

Repository files cannot define provider connections, replace executor programs,
raise concurrency, or expand secret-bundle access. They also cannot enable
prohibited commands or delivery, or redirect delivery and notification destinations.
Repository skill paths must remain inside their checkout. Account and runtime
grants remain separate administrator operations.

Application bundle names resolve through the project's private
`DATA_DIR/projects/PROJECT_ID/secrets.toml`, whose `[bundles]` table maps names
to private environment files. Relative bundle paths resolve beside that file.
The default project retains its user-level `secrets.toml`. Provider credentials
use the account store described below and do not belong in application bundles.

Provider login handoffs configure the default project's legacy credentials.
For a managed project account, import its credential with `account credential-set`
as shown below. Project-bound MCP connections cannot start a host-wide login
handoff or replace another project's credentials.

Create managed accounts using the provider's executor kind, authentication mode,
and endpoint. Save the returned account ID for inspection, grants, or pinning:

```sh
horde --project horde account create subscription-one --provider codex --auth-mode login --base-url https://api.openai.com/v1 --concurrency 2
horde --project horde account credential-set ACCOUNT_ID /private/path/credential.json
horde --project horde account list
horde account grant ACCOUNT_ID hamster
horde account revoke ACCOUNT_ID hamster
```

A credential file contains `kind`, `secret`, and optional `expires_at` (Unix
seconds) and `metadata`. Supported kinds are `api_key`, `claude_setup_token`,
`codex_refresh_token`, and `codex_access_token`. Keep this file private and outside
the repository. Importing a credential creates a new version; only the owning
project can replace it. Account inspection returns metadata without the secret.

For Codex, `secret` can contain the serialized authentication JSON with its
`tokens.refresh_token`, or the refresh token with the account and token fields
in `metadata`. An access-only token requires `metadata.account_id` and cannot
renew itself. For Claude, generate a token using `claude setup-token` and import
it as `claude_setup_token`; set its expiration when known. Horde reports expired
credentials and requires renewal.

Roles may pin `account = "ACCOUNT_ID"`. Without a pin, Horde chooses among the
project's granted accounts that match the resolved provider kind, authentication
mode, and endpoint. Add several matching accounts to spread invocations across
subscriptions. Accounts shared through grants retain one quota identity and
concurrency total. See [runtime management](runtime-management.md#project-capacity-and-credential-lifetimes)
for reservations, refresh, and revocation.

## Notifications

A `[notify]` table makes the daemon push task milestones to a webhook, a local
command, or both, so nothing has to poll `inspect`. Set `webhook` to a URL, or
`webhook_env` to the name of a variable that holds one and is read from the
daemon environment or `credentials.env` at send time, so a URL with a token
never enters a task's settings snapshot. `command` runs from the task's
repository with the JSON payload on stdin and `HORDE_TASK`, `HORDE_HOOK`, and
`HORDE_EVENT` in its environment.

```toml
[notify]
webhook_env = "HORDE_WEBHOOK_URL"
command = ["/bin/sh", "-c", "cat >> horde-notify.log"]
events = ["step.finished", "task.finished", "question.asked"] # also task.blocked
timeout_seconds = 15
children = false
```

`events` defaults to `step.finished`, `task.finished`, and `question.asked`;
`task.blocked` is the fourth hook and any other name fails at load.
`timeout_seconds` bounds one webhook request or command run and must be
positive. `children` extends delivery to delegated child tasks, which are silent
by default. Like every other setting, `[notify]` is pinned at submission. See
[progress and notifications](progress.md) for the payload, the delivery
records, and receiver examples.

## Step progress budgets

The daemon ends an attempt after `step_budget_seconds` without durable progress.
Defaults are 600 seconds for planners and reviewers, and 1800 seconds for workers.
A step's explicit value wins over its executor role's value, which wins over the
global default. All values must be positive seconds:

```toml
step_budget_seconds = 1800

[executors.planner]
step_budget_seconds = 600

[executors.reviewer]
step_budget_seconds = 600
```

An accepted plan proposal, a new artifact, or changed workspace files/commits
resets the window. Reads, messages, worker status changes, and identical writes
or duplicate artifacts do not. Command and harness file changes are observed by
periodic workspace scans; ignored files are excluded. This is a **progress timeout**:
a worker making changes can run longer than the configured number of seconds.
Total elapsed wall time continues to accumulate across resets.

The window covers workspace setup, model requests, tools, and integration.
Exhaustion stops owned commands, records `step.budget_exhausted`, and finishes the
attempt with `{"error":"step budget exhausted","elapsed_s":...,"budget_s":...}`.
The existing retry/fallback policy applies, with a fresh budget for each attempt.
`timeout_seconds` remains the separate request/command timeout and can fail an
operation earlier.

`horde inspect TASK_ID` adds `timing` to attempts: `elapsed_s`, `idle_s`, `budget_s`,
and `remaining_s`. `horde events TASK_ID` includes budget start, progress,
exhaustion, and finish events, plus timing on model responses and tool calls.
`horde metrics TASK_ID` reports a `steps` array with wall time, summed attempt time,
tokens, coordination counts, and attempt timing. Step wall time includes gaps
between retries; summed attempt time excludes those gaps.

A command that legitimately produces no durable changes for a long time can set
`step_budget_exempt = true` on its step. This disables only the progress budget;
it does not make output or heartbeat lines count as durable progress. Agent steps
cannot opt out. Exempt attempts still report elapsed time, with `budget_exempt: true`
and null budget/remaining values in their timing.

The separate `timeout_seconds` command limit still applies and defaults to 1800
seconds. For example, a silent GPU bench with a three-hour ceiling needs both:

```toml
# .horde/horde.toml
timeout_seconds = 10800
```

```toml
# .horde/templates/bench.toml
name = "bench"
version = "1"
[[steps]]
id = "gpu-cell"
kind = "command"
step_budget_exempt = true
command = ["bash", "tools/horde/fleet_gate_step.sh"]
```

Omit `step_budget_seconds` on an exempt step; setting both is an error. Set the
exemption directly on a command step, not on a template inclusion. Cancellation,
process cleanup, and retry handling still apply. Existing tasks keep their pinned
settings, so submit a new task after changing the template or command timeout.

## Knowledge topics

Optionally declare a vocabulary for the task family's notebook:

```toml
knowledge_topics = ["architecture", "testing", "performance"]
```

The default empty list permits free-form topics. A configured list permits at most
128 unique, nonempty strings of at most 128 bytes each. The root's pinned vocabulary
applies throughout its family. Workers see it in their tool schemas; administrative
clients can retrieve task-specific schemas through `knowledge_options`. See
[task-family notebooks](coordination.md#task-family-notebooks) for scope, queries,
claim lifecycle, and export.

## Project command environments

Native commands in a new project use a private home under
`DATA_DIR/projects/PROJECT_ID/command-state/home`. Their XDG configuration,
cache, data, and state directories live alongside that home. Command steps,
native worker tools, app processes, and Compose commands use these paths.
They do not inherit the host SSH agent. Application bundles cannot override
these directory settings. The migrated `default` project keeps its existing
command environment.

Provision tools and authentication that depend on a home directory in the
project's directories before running work. This includes GitHub CLI login,
Docker contexts, and language toolchains discovered through the home directory.
Executables still resolve through the daemon's `PATH`. A separate home does
not prevent a native command from reading other files accessible to the same
OS user; use VM isolation when that boundary is required.

Managed provider harnesses use separate homes and configuration directories
under `DATA_DIR/projects/PROJECT_ID/accounts/ACCOUNT_ID`. Codex refresh
credentials remain in the controller's private account storage. Worker
app-server sessions receive access tokens through the authenticated controller
connection and use the external-token login mode. Claude subscription sessions
use the selected setup token and an account-specific `CLAUDE_CONFIG_DIR`.
