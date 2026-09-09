# Configuration

`horde config init` writes a starter `config.toml` and a private `credentials.env`
into the configuration directory, creating neither if it is already there. The CLI
installer runs it, so a fresh install has a file to edit. `horde config` prints the
complete default TOML.

Settings merge in this order, later winning:

1. Built-in defaults.
2. `$XDG_CONFIG_HOME/horde/config.toml`, or `~/.config/horde/config.toml`.
3. `.horde.toml` in the submitted repository (legacy path).
4. `.horde/horde.toml` in the submitted repository (preferred path).

Later files override matching settings and retain settings they do not restate.
Both repository paths remain supported; if both exist, `.horde/horde.toml` wins.
To migrate, move `.horde.toml` to `.horde/horde.toml` after updating Horde.

The merged result is **pinned to the task at submission**. Later edits affect new
tasks only. Repository settings can restrict the daemon but never raise its
ceiling: concurrency, limits, and network trust stay user-owned.

## Defaults

```toml
concurrency = 4
autonomy = true
default_template = "local-implementation"
timeout_seconds = 1800
max_tool_rounds = 64
max_identical_tool_calls = 3
tool_event_bytes = 512
allow_commands = true
secret_bundles = []
```

- `concurrency` — daemon-wide worker ceiling, 1 to 64. A project may lower it for
  its own task.
- `autonomy = false` holds every new task until its initial question is answered
  with `horde answer TASK_ID QUESTION_ID yes`.
- `timeout_seconds` — per-invocation ceiling.
- `max_tool_rounds` caps native model/tool rounds per invocation.
- `max_identical_tool_calls` defaults to 3 consecutive calls with identical
  arguments and unchanged results. The next identical call holds the task for
  inspection. Zero disables the guard.
- `tool_event_bytes` limits each redacted tool argument/result field to 512 UTF-8
  bytes by default. The range is 0 through 65,536; zero omits both payloads.
- `allow_commands = false` removes the native `command` tool.

Live concurrency, without restarting or interrupting work:

```sh
horde config get concurrency
horde config set concurrency 8
horde runtime status
horde runtime drain      # stop new dispatches, let running work finish
horde runtime resume
```

## Adding a provider from the CLI

Installing offers this walkthrough, and it can be run again at any time. It keeps
asking until you say you are done, so several keys go in one sitting:

```sh
horde config init --interactive    # what the installer runs
horde config provider add          # pick a provider, a model, paste the key
horde config provider list         # what is configured, and whether each key reads
horde config models default        # what that endpoint will accept as a model
```

Prompts go to the controlling terminal rather than standard input, so they still
appear under `curl ... | sh`. With no terminal — a CI install, or an agent's tool
call — `config init --interactive` prints the command to run later rather than
failing, and `provider add` says to use the flags below.

Scriptable, with the key on standard input rather than an argument:

```sh
printf '%s\n' "$KEY" | horde config provider add tuara \
  --key-stdin --model qwen/qwen3.8-27b --use-for planner,worker,reviewer

horde config provider add claude --use-for reviewer     # login: no key prompted
```

Presets: `tuara`, `codex`, `claude` (subscription login), `openai`, `anthropic`
(the same CLIs against their API with a key). Anything else is described with
`--kind`, `--base-url`, and `--api-key-env`.

The key is never accepted as a command-line argument — it would land in the shell
history and in `ps` — so it is prompted for without echo, or piped in with
`--key-stdin`. It is written only to `credentials.env`; `config.toml` gets the
variable name. Adding a preset that matches a provider you already have configures
that one rather than leaving a duplicate behind; pass a different name to get a
genuinely separate provider. Comments and hand-written stanzas in `config.toml`
survive the edit, and settings that would no longer load are reported instead of
being left on disk.

## Providers

A provider is a named endpoint and credential, declared once. Every executor role
points at one, so an API key is written in exactly one place no matter how many
roles use it.

```toml
[providers.default]
kind = "tuara"
auth_mode = "api"
base_url = "https://tuara.com/router/v1"
api_key_env = "TUARA_API_KEY"
model = "qwen/qwen3.8-27b"    # what roles get unless they name their own
max_tokens = 8192
max_price = "1.00"             # dollars per million tokens, not a total budget
```

Preconfigured providers: `default` (Tuara over an API key), `codex` and `claude`
(the installed CLIs under subscription login), and `simulated`.

Nothing is inherited between providers. A provider that omits `base_url` or
`api_key_env` while it needs one is rejected at load rather than quietly picking up
another provider's endpoint or key.

`kind` is one of:

| kind | Behavior |
| --- | --- |
| `codex` | Runs the installed Codex CLI with workspace-write sandboxing and a preapproved coordination MCP server |
| `claude` | Runs the installed Claude Code CLI with explicit allowed tools and a scoped MCP config |
| `tuara` | Horde's own native tool loop against an OpenAI-compatible chat-completions endpoint |
| `simulated` | Returns a fixed accepted result; used to exercise scheduling with no model calls |

Model names for the harness kinds inherit their upstream default unless set
explicitly. For `tuara`, every invocation verifies the exact configured identifier
against `/models` and fails rather than substituting an alias. `model = "auto"`
selects exactly one nonempty catalog ID; zero or multiple entries fail and list
the available IDs. With `auto`, a provider can omit `kind` and `auth_mode` when it
supplies `base_url` and `api_key_env`:

```toml
[providers.default]
base_url = "http://127.0.0.1:8122/v1"
api_key_env = "LOCAL_MODEL_KEY"
model = "auto"
```

Set `stream = true` to decode SSE and emit first-token/tool-intent progress.
Provider and role `extra_body` tables carry options such as `temperature` and
`chat_template_kwargs`. Role keys override provider keys; nested objects are
replaced, not recursively merged. Reserved fields (`model`, `messages`, `tools`,
`stream`, `max_tokens`, `max_price`, and `n`) fail at configuration load. Use the
dedicated token and price settings. These options apply to native execution.

```toml
[providers.default.extra_body]
temperature = 0.2
chat_template_kwargs = { enable_thinking = false }

[executors.planner.extra_body]
temperature = 0.5
```

See the [native provider guide](https://github.com/gethamster/horde/blob/main/docs/native-providers.md) for the request
and telemetry contract.

## Executor roles

A role is a name a template step asks for. A role chooses two things: which provider
it goes through, and which model it asks that provider for.

```toml
[executors.worker]                 # provider omitted: providers.default
model = "a-fast-model"

[executors.reviewer]
provider = "claude"                # a different endpoint and credential
max_tokens = 16384
```

Preconfigured roles: `planner`, `worker`, `reviewer`, `native` (all on
`providers.default`), plus `codex`, `claude`, and `simulated` on the providers of
the same name.

Besides `provider` and `model`, a role may set `account`, `program`, `max_price`,
`max_tokens`, and `max_api_cost_usd`; each falls back to the provider's value.
Native roles also accept the `extra_body` overrides described above.

A role cannot restate `kind`, `auth_mode`, `base_url`, or `api_key_env` — those four
travel together on a provider, so no role can pair one provider's harness with
another's key. Setting them on a role is an error that names the provider to declare
instead.

## Authentication

Two modes, per provider.

**Subscription login (`auth_mode = "login"`).** Codex and Claude use their own
installed CLI and existing credential store. Horde does not read or copy those
credentials.

**API keys (`auth_mode = "api"`, the default provider's mode).** Set `base_url` and
`api_key_env` on the provider:

```toml
[providers.anthropic]
kind = "claude"
auth_mode = "api"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"

[providers.openai]
kind = "codex"
auth_mode = "api"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
```

The named variable must be set in the **daemon's** environment before
`horde start`, or in a private mode-0600 `credentials.env` beside `config.toml`
for a service install.

Real keys are never injected into harness child environments. For each invocation
Horde starts a loopback broker that holds the real key, issues the harness a
temporary credential, restricts requests to that invocation's model endpoints,
passes SSE and provider errors through unchanged, and revokes the token when the
invocation completes. Worker command environments use an allowlist that omits
provider keys entirely.

Budget controls are honest about what they can enforce. `max_price` is a per-token
price ceiling. Claude's CLI budget option is passed through when configured. A
total-spend cap that a provider does not support fails explicitly rather than being
silently ignored.

## Fallbacks

A role can escalate to another configured role after a failed attempt:

```toml
[fallbacks]
worker = "reviewer"
```

With `attempts = 2` on a step, the first attempt uses `worker` and the second uses
`reviewer`. Fallbacks are opt-in, cycle-checked, and recorded as escalation events.
Missing roles and cycles are rejected at load. Nothing falls back without a mapping;
this is a configured change of executor, not a silent model alias.

Capacity-aware routing uses the same chain: at the configured switch threshold new
invocations follow the fallback chain, and if every alternative is unavailable the
work stays queued. Unknown capacity permits execution. See `fleet.md`.

## Delegation limits

```toml
[limits]
workers = 4          # active workers in the whole tree
children = 16        # total child tasks ever created, including finished ones
depth = 3            # levels below the root
environments = 2     # concurrent app environments
```

Pinned at submission and inherited. Children cannot reset them. See
`delegation.md`.

## App secret bundles

```toml
secret_bundles = ["app"]
```

Names a bundle defined in `~/.config/horde/secrets.toml`. These are credentials for
the software being built and tested, never provider API keys. See
`environments.md`.

## Runtime skills

Configure `[skills]` as a mapping from names to directories containing `SKILL.md`.
A step selects names with `skills = ["name"]`. Submission pins the configured
bundles, so later source changes affect new tasks. Children inherit the catalog;
`delegate_task.skills` narrows it and selects skills for child agent steps.
See [runtime skills](https://github.com/gethamster/horde/blob/main/docs/runtime-skills.md) for resource access and limits.

## Delivery

Off by default. See `delivery.md` before enabling.

```toml
[delivery]
enabled = true
repository = "owner/repo"
base = "main"
merge = true
deploy_workflow = "deploy.yml"
health_url = "https://example.com/health"
```

## Inspecting the merged result

```sh
horde doctor --repo /path/to/repo
```

This prints the merged settings and resolves native `auto` catalogs without a
chat-completions request. `horde doctor --provider NAME` checks one provider;
`horde doctor --probe --provider NAME` also requests a streamed tool call and
consumes model capacity.
