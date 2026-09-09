# Configuration

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
3. `.horde.toml` in the submitted repository (legacy path).
4. `.horde/horde.toml` in the submitted repository (preferred path).

Later files override matching settings and retain settings they do not restate.
Both repository paths remain supported; if both exist, `.horde/horde.toml` wins.
To migrate, move `.horde.toml` to `.horde/horde.toml` after updating Horde.

The merged settings are pinned to the task. Later file changes affect new tasks.

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

The shipped `grok` provider runs the Grok CLI (`grok -p PROMPT --output-format json --always-approve`, `kind = "grok"`) under its installed subscription login; `auth_mode = "api"` is refused for it. The harness writes the coordination MCP server into the workspace's `.grok/config.toml` (the shape `grok mcp add --scope project` writes), passes the prompt as a command-line argument (limit 200 KiB), and reads the single JSON reply. Use it as a fallback hop, for example `[fallbacks] codex = "grok"`.

Set `autonomy = false` to hold new tasks until the initial question is answered:

```sh
horde answer TASK_ID QUESTION_ID yes
```

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
