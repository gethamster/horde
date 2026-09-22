# Operation reference

Every operation below is available two ways with identical arguments and results:

```sh
horde call OPERATION '{"json":"object"}'
```

or as an MCP tool on the `horde mcp` bridge. Arguments must be a JSON object.
Results are JSON.

Notation: `*` marks a required field. `task` identifies the task and is required
for every task-scoped operation when you call it from the CLI or the
personal-agent bridge. A worker token supplies `task` and `worker` itself and
rejects any attempt to name a different one. The tables below show operator
arguments. Worker schemas omit runtime identity, step attribution, and verification
fields; use the [worker reference](https://github.com/gethamster/horde/blob/main/skills/horde-worker/references/operations.md)
for those calls.

## CLI shortcuts

| Command | Operation |
| --- | --- |
| `horde submit OBJECTIVE --repo P --template T` | `submit_task` |
| `horde list` | `list_tasks` |
| `horde inspect ID` | `inspect` |
| `horde events ID --after N` | `events` |
| `horde metrics ID` | `metrics` |
| `horde cancel ID` | `cancel` |
| `horde resume ID` | `resume` |
| `horde answer ID QID ANSWER [--human]` | `answer_question`; `--human` sends `human: true` |
| `horde config get/set concurrency N` | `runtime_config_get` / `runtime_config_set` |
| `horde usage` | `account_status` |
| `horde runtime status/drain/resume/list/inspect/create/...` | `runtime_*` |

Other commands have no operation equivalent: `init` (repository setup), `start`, `stop`, `daemon`, `mcp`,
`doctor`, `validate`, `config` (print), `service`, `update`, `network`.

## Task lifecycle

| Operation | Arguments | Notes |
| --- | --- | --- |
| `submit_task` | `objective`*, `repo`*, `template`, `context[]` | Returns `{"id":...}`. `repo` is an absolute path. `context` entries are context records. |
| `list_tasks` | none | |
| `inspect` | `task`* | Steps, attempts, workers, questions, integration state. |
| `events` | `task`*, `after`, `consumer` | Ordered activity events. |
| `ack_events` | `task`*, `consumer`*, `seq`* | Durable, monotonic per-consumer receipt. Does not answer questions. |
| `metrics` | `task`* | Usage, cost, latency, retries, coordination counts, `children` tree. |
| `cancel` | `task`* | Kills active process groups; cancels the owned subtree. |
| `resume` | `task`* | Interrupted processes must be reconciled first. |
| `add_steps` | `task`*, `steps`* | Appends a validated workflow revision. |

## Questions

| Operation | Arguments | Worker |
| --- | --- | --- |
| `request_question` | `task`*, `worker`, `question`*, `id`, `evidence`, `recommendation`, `human_only` | yes |
| `pending_questions` | `task`* | yes |
| `answer_question` | `task`*, `question`*, `answer`*, `worker`, `human` | yes |
| `escalate_question` | `task`*, `question`*, `commentary`, `worker` | yes |

## Delegation

| Operation | Arguments | Worker |
| --- | --- | --- |
| `delegate_task` | `task`*, `id`*, `objective`*, `template`, `peer`, `bundles[]`, `skills[]`, `worker` | yes |
| `list_children` | `task`* | yes |
| `integrate_child` | `task`*, `child`*, `validation[]`*, `worker` | yes |
| `read_context` | `task`*, `after`, `limit` | yes |
| `update_context` | `task`*, `content`*, `provenance`*, `kind`, `id`, `mandatory`, `supersedes[]` | no (root caller only) |

## Coordination

| Operation | Arguments | Worker |
| --- | --- | --- |
| `register_worker` | `task`*, `step` | no |
| `register_workspace` | `task`*, `worker`, `path`*, `branch`*, `base`* | yes |
| `list_workers` | `task`* | yes |
| `set_worker_status` | `task`*, `worker`, `status`* (`idle`/`working`/`blocked`/`stopped`) | yes |
| `claim_paths` | `task`*, `worker`, `paths[]`* | yes |
| `transfer_claim` | `task`*, `worker`, `to`*, `path`* | yes |
| `release_claims` | `task`*, `worker`* | no |
| `reconcile_worker` | `task`*, `worker`* | no |
| `send_message` | `task`*, `worker`, `id`*, `destination`*, `body`*, `refs{}`, `actionable` | yes |
| `steer` | `task`*, `body`*, `id`, `refs{}`, `actionable`, `worker` | no (operator) |
| `read_messages` | `task`*, `worker`, `after`, `limit` | yes |
| `acknowledge_messages` | `task`*, `worker`, `ids[]`* | yes |
| `join_channel` | `task`*, `worker`, `channel`* | yes |
| `integrate` | `task`*, `worker`, `validation[]` | no |
| `propose_steps` | `task`*, `worker`, `steps`* | yes (planner) |

## Artifacts and knowledge

| Operation | Arguments | Worker |
| --- | --- | --- |
| `put_artifact` | `task`*, `name`*, `content`*, `inputs{}`, `verified`, `worker`, `step` | yes; supplied `verified` is dropped |
| `get_artifact` | `task`*, `hash`* | yes |
| `reuse_artifact` | `task`*, `name`*, `inputs{}`* | yes |
| `add_knowledge` | `task`*, `kind`*, `content`*, `provenance{}`*, `inputs{}`, `verified`, `step` | yes; supplied `verified` is dropped |
| `knowledge` | `task`* | yes |
| `link_knowledge` | `task`*, `source`*, `target`*, `relation`* | yes |

## Pinned skills

| Operation | Arguments | Worker |
| --- | --- | --- |
| `list_skills` | `task`* | yes |
| `read_skill` | `task`*, `name`*, `path`, `offset`, `limit` | yes |

`read_skill` defaults to `SKILL.md` and returns bounded resource pages. Follow
`next_offset` when present. The task's pinned catalog also bounds child inheritance.

## Environments and secrets

| Operation | Arguments | Worker |
| --- | --- | --- |
| `environments` | `task`* | yes |
| `refresh_bundles` | `task`* | no |

## Runtime and fleet management

None of these are available to a worker token.

| Operation | Arguments |
| --- | --- |
| `runtime_config_get` | none |
| `runtime_config_set` | `concurrency`* |
| `runtime_status` | none |
| `runtime_drain` / `runtime_resume` | none |
| `runtime_list` | none |
| `runtime_inspect` | `id`* |
| `runtime_create` | `id`*, `profile`*, `request_id`* |
| `runtime_destroy` / `runtime_restart` / `runtime_start` / `runtime_stop` | `id`*, `request_id`* |
| `runtime_update` | `id`*, `request_id`*, `version`* |
| `runtime_skills_update` | `id`*, `request_id`* |
| `skill_pack_list` | none |
| `skill_pack_install` | `path`* (absolute pack directory) |
| `runtime_reconcile` | `id`*, `request_id`*, `resource`* |
| `runtime_updates_resume` | none |
| `account_status` | none |
| `account_observe` | `account`*, `provider`*, `window`*, `observed_at`*, `source`*, `used_percent`, `reset_at` |
| `management_events` | `after` |
| `management_ack` | `consumer`*, `seq`* |

## Provider account setup

These operations require an administrative agent connection. They make no model
request, so exhausted model quota does not prevent a connected agent from
changing credentials.

| Operation | Arguments | Result |
| --- | --- | --- |
| `agent_setup` | `action: "configure_provider"`, `provider`*, optional `credential`, `credential_env`, or `credential_file`; optional provider settings and `roles[]` | Saves the provider and key. Choose at most one credential source. The response omits the key and reports activation separately from verification. |
| `provider_login` | `action: "start"`, `provider`*, `request_id`*, `timeout_seconds` | Starts a Tuara key-page handoff, `codex login --device-auth`, or `claude auth login`; returns a `session_id`. The timeout defaults to 600 seconds and accepts 1 through 1800. |
| `provider_login` | `action: "status"`, `session_id`* | Returns status, bounded `output`, expiry, and authentication evidence. Relay the login instructions to the user. `method` is `api_key` for Tuara or `cli_login` for Codex/Claude. |
| `provider_login` | `action: "submit"`, `session_id`*, `input`* | Accepts one nonempty line of at most 4096 bytes: a Tuara inference API key or a CLI-requested authorization code. |
| `provider_login` | `action: "cancel"`, `session_id`* | Cancels the session and stops any process group or pending verification request. |
| `provider_wallet` | `action: "inspect"` | Reads the local Link wallet readiness and returns the next setup action when one is needed. |
| `provider_wallet` | `action: "install"`, `request_id`*; optional `timeout_seconds` | Starts a bounded, supervised private Link installation. Reuse the request ID after a lost reply. |
| `provider_wallet` | `action: "login_start"`, `request_id`*; optional `timeout_seconds` | Starts a bounded Link device-login session and returns its verification URL, device phrase, and session ID. |
| `provider_wallet` | `action: "status"` or `"login_status"`, `session_id`* | Reads the current installation or login session without starting another one. |
| `provider_wallet` | `action: "cancel"` or `"login_cancel"`, `session_id`* | Stops an unfinished Link installation or login session. |
| `provider_wallet` | `action: "details"` | Returns safe wallet readiness and the fixed [Link Wallet](https://app.link.com/wallet) URL for payment details or verification. It never returns PANs, CVCs, or full payment details. |
| `provider_signup` | `action: "start"`, `request_id`*, `provider`*, `organization_name`*, `agent_name`*, `amount_cents`*, `max_charge_cents`*, `terms_version`*, `accept_terms: true`; optional `replace_existing` | Validates a Tuara signup quote without paying. Amounts are US cents; the maximum total including fees is 50,000. Existing credentials require explicit replacement authorization. |
| `provider_signup` | `action: "status"`, `request_id`* | Reads durable local signup progress without contacting Tuara or the wallet. Returns the quote, approval URL when available, and next actions; never returns the key or payment token. |
| `provider_signup` | `action: "resume"`, `request_id`* | Advances a bounded step: creates or checks a Link wallet approval, submits one approved paid signup, or verifies and imports a privately saved key. |
| `provider_signup` | `action: "cancel"`, `request_id`* | Cancels Horde's signup and its unpaid Link authorization before payment submission. Does not reverse a submitted payment. |
| `provider_topup` | `action: "configure"`, `provider`*, `threshold_cents`*, `amount_cents`*, `max_charge_cents`*, `monthly_limit_cents`*, `terms_version`*, `accept_terms: true` | Creates or replaces a recurring Tuara policy after explicit authorization. Per-charge and monthly limits include fees; the monthly limit applies to the UTC calendar month. |
| `provider_topup` | `action: "status"` or `"check"`, `provider`* | Reads the durable policy, or advances one bounded funding phase without waiting for the next daemon tick. |
| `provider_topup` | `action: "disable"`, `provider`* | Cancels unpaid pending work and future checks. It does not reverse submitted payments or erase the spending ledger. |

`provider_wallet` is available on the same administrative connection as provider
setup. An agent starts with `inspect`, then follows the returned bounded install
and device-login actions. Relay the Link verification URL and phrase, and direct
the user to the fixed Link wallet URL returned by `details`. Link hosts wallet and
identity changes; MCP payment fields never include PANs, CVCs, or full card
details. A previously used Link account, including one used with Grok Bot, can
be reused. Horde does not register or configure a Link MCP server in Grok Bot.

`provider_signup` requires an unbound default-project administrative connection
and a ready Link wallet. Worker and project-scoped connections cannot call it.
Use it only after the operator authorizes signup,
the maximum charge including fees, and a specific terms version. Human operators
use `horde config provider signup tuara`; agents construct the internal operation
arguments and do not ask users to write JSON.

Signup states include `preparing`, `awaiting_wallet`, `awaiting_approval`,
`submitting`, `credential_received`, `succeeded`, `failed`, `cancelled`,
`expired`, and `uncertain`. Relay the approval URL for the individual payment and reuse the same
request ID for bounded resumes. If `wallet_action_required` is true, resolve the
action in Link before another resume instead of repeatedly polling.
`credential_received` needs another resume to
verify and install the saved key; `succeeded` reports verified authentication
and activation on the next invocation. Capacity remains unknown. Receipts are
private and survive daemon restarts. An uncertain paid outcome requires Tuara
and wallet reconciliation; Horde will not replay it. Repeating `start` with
identical arguments returns the saved operation, while changed arguments fail.

`provider_topup` has the same administrative, default-project, and ready-wallet
requirements. Its daemon check runs every 60 seconds and waits five minutes
after a successful charge. The policy keeps fee-inclusive charges in a UTC
monthly ledger and shares that ledger with aliases for the same Tuara origin and
verified organization within one configuration directory. It does not limit
spending outside that Horde policy. A Link action may be required before an
approved charge can submit. Pending and uncertain charges hold later payments;
an uncertain outcome needs Tuara and Link reconciliation and is never replayed.
Human operators use `horde config provider topup tuara`, plus `--status`,
`--check`, or `--disable`. See [the signup and top-up guide](../../../docs/configuration.md#create-a-funded-tuara-account).

`credential_activation: "next_invocation"` and `restart_required: false` mean
the saved API key applies to the next invocation. Explicitly supplied credentials
take precedence over older daemon environment values for those variables.
Running invocations retain their credentials. Saving a key leaves
`provider_api: "not_probed"`.

Login status is `starting`, `awaiting_user`, or `verifying` until a terminal result
of `succeeded`, `failed`, `expired`, or `cancelled`. `succeeded` requires a successful
login and CLI authentication-status check, or Tuara API-key introspection with
`router:invoke` scope followed by saving the key. `provider_authentication` then reports
`verified`; `capacity` remains `unknown`. CLI output is provider-supplied text,
and submitted input is redacted from returned output. Tuara returns authored
instructions only, never the submitted key or upstream response body. Its
successful session also reports `credential_activation: "next_invocation"`.

Retry `start` with the same request ID and arguments after a lost reply. Horde
retains the session until its deadline plus one hour while the daemon lives.
Sessions survive client disconnects but not daemon restart, which never replays
login input. Only one active login per provider kind is allowed on the runtime;
Codex and Claude invocations share their CLI's account store.

An effective API key change or verified login invalidates affected provider quota
observations and preserves local budgets. Existing task provider bindings and
uncertain-work recovery requirements remain in force.

## Connecting fleet workers

To connect and use a named worker, configure the controller network once, then:

```sh
# Controller: for a reachable Tailscale peer, one command creates and
# Taildrops a scoped credential, and prints the exact join command to run there.
horde network invite apollo
```

Without a discoverable Tailscale peer, use the manual two-step credential
flow instead:

```sh
# Controller: creates workers.json with inferred addresses and trust.
horde network key create workers
# Worker: provide workers.json through a private file or secret mount.
horde network join workers.json --name apollo
```

Either way, once the worker is connected:

```sh
# Controller: submit a whole task without creating a parent first.
horde submit --on apollo --repo /path/to/repo "Implement and test the change"
horde result TASK_ID
```

`submit_task` accepts an optional `on` runtime name or ID. `remote_result` takes a
`task` ID and returns a separate checkout for review, preserving the original
repository. `runtime_rename` takes `id` and `name`; `runtime_forget` takes `id` and
only removes disconnected entries without unfinished work. Names never replace
authenticated runtime IDs. `horde runtime list --json` returns full records.

Use `horde network key list`, `horde network key revoke KEY_ID`, and
`horde network revoke WORKER_ID` to manage admissions and membership. For fleet
startup, inject `HORDE_ENROLLMENT_FILE` or `HORDE_ENROLLMENT_JSON` and run
`horde daemon`. Expired certificates can recover with the original valid fleet
credential; revoked workers cannot recover. The launching platform owns the
resource lifecycle. See [fleet credentials](../../../docs/networking.md#fleet-credentials-when-you-cant-ssh-in)
for limits and secret delivery.

## Worker token scope

A worker token may call exactly these, and nothing else:

`integrate_child`, `environments`, `delegate_task`, `list_children`,
`read_context`, `pending_questions`, `escalate_question`, `answer_question`,
`propose_steps`, `request_question`, `send_message`, `read_messages`,
`acknowledge_messages`, `list_workers`, `set_worker_status`, `join_channel`,
`claim_paths`, `transfer_claim`, `register_workspace`, `put_artifact`,
`get_artifact`, `reuse_artifact`, `add_knowledge`, `knowledge`, `link_knowledge`,
`list_skills`, `read_skill`.

Additional worker restrictions, enforced at the RPC layer:

- `task` and `worker` are forced to the token's own identity.
- A worker cannot attribute work to another worker's step.
- A worker-supplied `verified` field is dropped with a warning. Evidence remains
  unverified; certification is an authoritative runtime decision.
- Arguments starting with `_` are rejected as reserved.

These are scope limits at the RPC layer, not process isolation. A worker token
does not protect against a malicious local process running as the same user.

For `add_knowledge`, `kind` must be `fact`, `decision`, or `evidence`.

Notebook tools are pull-based. `add_knowledge` defaults to private task scope;
set `scope: "family"` to publish within the task tree. Use `knowledge` with explicit
scope, optional `topic`/FTS5 `query`, and `limit`; follow `next` with `after`. Restart
pagination if the notebook revision changes. `knowledge_options` returns the pinned
topic vocabulary and task-specific schemas. `knowledge_edges` continues a row's
relationship pages when `edges_next` is present. Conditions in `valid_under` are
writer assertions, not runtime conclusions. Supersede owned claims through
`add_knowledge.supersedes` or withdraw them with `retract_knowledge`; use
`include_inactive: true` to inspect history. Knowledge never unblocks execution.
