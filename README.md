# Horde

[Source](https://github.com/gethamster/horde) · [Website](https://horde.sh) · [Apache-2.0 license](LICENSE)

A local Rust daemon for assigning software tasks to coding agents. Submit work through the CLI or connect your own agent through the stdio MCP bridge.

**Early release:** SQLite coordination, templates, worktrees, native Tuara execution, Codex/Claude adapters, bounded local/remote delegation, inherited context and app secrets, disposable process/Compose environments, and configured GitHub delivery are implemented. See [verification and limitations](docs/verification.md) for what has been exercised with real services versus fixtures.

## Quick start

Requires macOS or Linux, Git, and Rust (the repository pins its tested toolchain). Native search also uses `rg`. Real workers require a configured executor and its existing login or daemon-side API key. If Cargo is not on your shell path after installing Rust, run `source "$HOME/.cargo/env"`.

```sh
cargo install --path . --locked
horde start
horde submit "Add CSV export with tests" --repo /path/to/repository
horde inspect TASK_ID
horde events TASK_ID
horde metrics TASK_ID
```

The source repository must have an initial commit and a Git author identity. Coding happens in separate worktrees. The integrated result is on `horde/TASK_ID`; your original checkout stays on its existing branch. Use `git worktree list` to locate results. No push or delivery happens unless enabled in settings.

Try the complete scheduling path without model calls:

```sh
horde submit "Exercise the runtime" --repo . --template simulated
```

`horde stop` shuts the service down gracefully. `horde daemon` runs in the foreground; `horde start` detaches it and writes `daemon.log`. The default data directory is `~/.local/share/horde`. Set `--data-dir PATH` consistently on every command to use a different instance. Keep this path short enough for a Unix socket (under roughly 90 characters on macOS).

## Settings

Defaults are autonomous execution, four concurrent workers, Tuara over an API key for planning/implementation/review, and no delivery. `codex` and `claude` roles are configured for those CLIs under subscription login. Model names for the harnesses inherit their upstream defaults unless explicitly set.

Installing runs `horde config init`, which writes a starter `config.toml` and a private `credentials.env` into the configuration directory. It creates neither if it is already there, so it is safe to run again. `horde config` prints the complete default TOML.

Add a provider and its key without editing either file:

```sh
horde config provider add          # pick a provider, a model, paste the key
horde config provider list         # what is configured, and whether each key reads
horde config models default        # what that endpoint accepts as a model
```

Presets are `tuara`, `codex`, `claude`, `openai`, and `anthropic`; anything else is described with `--kind`, `--base-url`, and `--api-key-env`. For scripts, `printf '%s\n' "$KEY" | horde config provider add tuara --key-stdin --use-for planner,worker,reviewer`. The key is never taken as a command-line argument, and lands only in `credentials.env`.

Settings merge in this order:

1. Built-in defaults.
2. `$XDG_CONFIG_HOME/horde/config.toml`, or `~/.config/horde/config.toml`.
3. `.horde.toml` in the submitted repository.

The merged settings are pinned to the task. Later file changes affect new tasks.

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

The default provider asks Tuara for `qwen/qwen3.8-27b`. Every native invocation verifies its exact configured identifier against `/models`; unavailable models fail without substitution. `horde doctor --probe-tuara` checks the catalog and performs a small streaming tool-call probe. The native loop uses non-streaming tool calls with string content. See [provider evidence](docs/verification.md).

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

Set `autonomy = false` to hold new tasks until the initial question is answered:

```sh
horde answer TASK_ID QUESTION_ID yes
```

## Connect your agent

Configure a stdio MCP server with command `horde` and arguments `mcp`. For a custom data directory, arguments are `--data-dir`, `/absolute/path`, `mcp`.

```json
{
  "mcpServers": {
    "horde": { "command": "horde", "args": ["mcp"] }
  }
}
```

The personal-agent bridge exposes submit, inspect, events, questions, cancellation, resumption, metrics, revisions, artifacts, knowledge, and coordination tools. Internal harness bridges receive a worker token and expose only worker-scoped operations. Native tools and external MCP tools use the same coordination handlers.

For a script or independent harness, create a worker with `register_worker`, then register its separate worktree using `register_workspace` (`path`, `branch`, `base`). Set the returned token as `HORDE_WORKER_TOKEN` in its MCP bridge environment. Acquire claims before editing. Never share the personal-agent bridge with an untrusted worker.

## Coordination

Every operation is available as `horde call OPERATION 'JSON'`. Examples:

```sh
horde call register_worker '{"task":"TASK_ID"}'
horde call list_workers '{"task":"TASK_ID"}'
horde call claim_paths '{"task":"TASK_ID","worker":"WORKER_ID","paths":["src/api"]}'
horde call send_message '{"task":"TASK_ID","worker":"WORKER_ID","id":"unique-client-message-id","destination":"OTHER_WORKER_ID","body":"The response now includes a cursor","refs":{"file":"src/api.rs"},"actionable":true}'
horde call read_messages '{"task":"TASK_ID","worker":"WORKER_ID"}'
horde call acknowledge_messages '{"task":"TASK_ID","worker":"WORKER_ID","ids":["unique-client-message-id"]}'
```

Destinations are a worker ID, `group:NAME`, or `task`. Join a group with `join_channel`. Broadcast recipients are snapshotted at send time. Retrying the same message ID with the same payload is idempotent; changing its payload is rejected. Acknowledgement is explicit and per recipient, with a cursor that never skips unread mail.

Actionable messages notify idle managed workers and create a follow-up step, retaining worker identity. Presence messages and acknowledgements do not invoke models. Native workers receive unread messages at each model/tool round; harnesses receive a launch prompt and coordination MCP tools. Continuous push into an already-running CLI harness is not available in this release.

Claims use repository-relative files or directory prefixes. `.` claims the whole repository. Overlap is rejected with ownership evidence. `transfer_claim` is atomic. Claims survive crashes; conversation alone never transfers ownership. Native file and patch tools check claims. External edits and native commands are checked before integration, with out-of-scope results held for reconciliation.

## Templates

Built-ins: `local-implementation`, `nextjs`, `github-actions`, and `simulated`. Project templates in `.horde/templates/*.toml` may add or override them. Validate without executing:

```sh
horde validate github-actions --repo /path/to/repo
```

Templates declare versioned steps, roles, scopes, acceptance criteria, tools, expected artifacts, dependencies, output references, and nested templates. Compilation rejects missing inputs, duplicate IDs, missing dependencies, cycles, and recursive inclusion. Template content hashes and expanded plans are saved for every active task.

`${step.result}` supplies a dependency's named output. Nested templates namespace their steps and rewrite references. Steps with independent dependencies run concurrently when their write scopes permit it. A `when` condition references a dependency's terminal status. `attempts` bounds retries; each attempt retains separate evidence. Failure branches support repair followed by repeated verification. See [template examples](docs/templates.md).

`add_steps` validates and appends a workflow revision without rewriting earlier attempts. The personal agent can use it to evolve a workflow after inspecting evidence. An active planner can also use `propose_steps`: the runtime validates its proposed graph and inserts the new work before pending implementation/review steps.

## Delegation, context, and application testing

The root owns one bounded tree: four active workers, sixteen total child tasks,
three levels below the root, and two app environments by default. Children inherit
these limits, original intent, source references, answers, and selected app bundles.
A narrower child assignment does not replace the original request. Context changes
invalidate old acceptance and require fresh verification.

```sh
horde call update_context '{"task":"ROOT_ID","content":"Never export email addresses","provenance":"Original caller message 7","kind":"constraint"}'
horde call delegate_task '{"task":"ROOT_ID","id":"export-once","objective":"Implement CSV export","template":"local-implementation"}'
horde call pending_questions '{"task":"ROOT_ID"}'
horde call integrate_child '{"task":"ROOT_ID","child":"CHILD_ID","validation":["npm","test"]}'
```

The immediate caller can answer a child question or call `escalate_question` to
forward its original envelope one level. Your personal agent, Grokbot, or Slack
integration remains the external root caller through CLI/MCP; Horde does not
own that chat interface. See [delegation and caller integration](docs/delegation.md).

Select a named private `.env` bundle once and descendants inherit it, with optional
narrowing. Managed app steps inject it, start a process or isolated Compose project,
wait for readiness, run tests, retain redacted evidence, and tear down owned resources.
See [application environments and secrets](docs/environments.md) for runnable
configuration examples and recovery behavior.

## Delivery

`github-actions` composes Next.js verification with GitHub delivery. Enable delivery explicitly:

```toml
[delivery]
enabled = true
repository = "owner/repo"
base = "main"
merge = true
deploy_workflow = "deploy.yml"
health_url = "https://example.com/health"
```

Authenticate `gh` using its credential store first. The configured repository must match the checkout's `origin`. The runtime pushes the result branch, finds or creates its PR, watches checks, compares the verified head before merging, observes the configured **push-triggered** deployment for the merge commit, and checks health. Set `merge = false` to stop at a checked PR. It never force-pushes, bypasses branch protection, or dispatches duplicate deployment jobs.

External operation intents and returned identities are durable. PR and merge retries inspect GitHub state first. An unexpected head, a closed PR, conflicts, or failed health checks remain failures with evidence.

## Recovery and operations

```sh
horde cancel TASK_ID
horde inspect TASK_ID
horde call reconcile_worker '{"task":"TASK_ID","worker":"WORKER_ID"}'
horde resume TASK_ID
```

A hard daemon crash marks active attempts uncertain and blocks their tasks. `reconcile_worker` refuses while a recorded worker process is alive. Inspect the worktree and external effects, stop orphan processes, then reconcile and resume. This intentionally avoids replaying uncertain shell/model operations automatically. Claims remain available for handoff or explicit release after the process is reconciled. A graceful stop or cancellation kills each active process group.

Git integration is serialized per task. Merge conflicts are aborted without changing the existing integrated result; conflicting files and commits are sent back to the owner. A failed combined validation holds the combined changes for repair. Worktrees are retained for inspection.

## Optional runtime networking

Tailscale discovery and tonic/rustls mutual-TLS connectivity are available through
`horde network peers`, `listen`, and `probe`. Networking is disabled by default
and uses a separate user-owned trust configuration. Enrolled runtimes can execute child tasks, exchange committed snapshots,
route questions through their callers, and verify returned changes.
See [networking setup](docs/networking.md).

To connect your own machines, run `horde network setup`, then
`horde network add user@worker` for a discovered Tailscale SSH host. Horde
handles remote installation, certificates, enrollment, and the authenticated
handshake. See [automatic network setup](docs/networking.md#automatic-setup-and-pairing)
for initial tailnet access and boot-service options.

## Managed runtimes and installation

Horde can provision E2B, Daytona, Docker, and Kubernetes runtimes from user-owned
profiles and enroll remotes over an outbound mTLS control connection. Each runtime
has an independent concurrency limit. Account-capacity observations can select
configured fallback roles before dispatch. Administrative CLI/MCP tools expose
runtime lifecycle, quota status, drain/restart, and versioned updates.

See [runtime management](docs/runtime-management.md) for profiles, credentials,
usage collectors, enrollment, and recovery. See [installation and releases](docs/installing.md)
for the signed installer, machine-boot service opt-in, and release CI setup.

## Agent skills

Three skills teach a coding agent what Horde is and how to drive it. They are
maintained in [`skills/`](skills/) and published from the public
[`asomervell/horde-skills`](https://github.com/asomervell/horde-skills) repository:

```sh
npx skills add asomervell/horde-skills
```

`horde` covers installation, MCP wiring, submission, monitoring, and recovery.
`horde-templates` covers template authoring. `horde-worker` covers the coordination
protocol for an agent running inside a task. Update the matching reference file
whenever a change alters the CLI, an operation's arguments, or a default. See
[`skills/README.md`](skills/README.md).

## Development

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests use real SQLite databases, independent daemon/CLI/MCP processes, real Git worktrees/conflicts, and local HTTP/harness fixtures. `python3 scripts/live_smoke.py codex` (or `claude`) runs an opt-in, isolated live-provider task and may consume paid or subscription capacity. Native `tuara` and `local` runs require an explicit model; see the [smoke harness instructions](docs/verification.md#running-the-smoke-harness). See [architecture](docs/architecture.md) and [verification](docs/verification.md).

See the [contributing guide](CONTRIBUTING.md) for development expectations and the
[changelog](CHANGELOG.md) for release history.

The horde.sh landing page lives in `website/`. See its [development and hosting
instructions](website/README.md) for the Next.js build and static export.

The site also publishes the agent-facing description of Horde: Horde discovery
metadata at `/.well-known/mcp.json`, the tool catalog at
`/.well-known/tools.json`, and `/llms.txt`. The discovery file uses Horde's own
format and describes the installed stdio MCP server; it is not an official MCP
manifest or an HTTP MCP endpoint. The site exposes no API of its own. The tool
catalog is generated from `src/protocol.rs`; regenerate and commit it with
`UPDATE_WEBSITE_SPEC=1 cargo test --test website_spec` whenever the operation
set changes, or `cargo test` will fail.

Apache-2.0 licensed. Single-user and local-first; no dashboard or distributed scheduler.
