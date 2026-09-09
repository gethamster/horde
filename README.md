# Horde

Running several coding agents can leave you coordinating their work by hand:
passing context between sessions and checking whether their changes fit together.

Horde runs software tasks through planning, implementation, and review. Each agent
gets its own Git worktree. Horde combines their committed changes and keeps a
record you can inspect when a task fails or needs your input.

Use it from your terminal or connect an agent through MCP. Horde is open source,
runs on macOS and Linux, and works with Codex, Claude Code, or an OpenAI-compatible
model server, including one you run locally.

[Quick start](#quick-start) · [Documentation](docs/README.md) · [Website](https://horde.sh) · [Issues](https://github.com/gethamster/horde/issues) · [Apache-2.0](LICENSE)

## When to use Horde

Horde is useful when a change benefits from separate implementation and review,
or when independent parts can be assigned to different workers. For example:

| Task | How Horde helps |
| --- | --- |
| Add a feature across an API and its UI | Give each part a separate worktree, then check the combined result. |
| Refactor a module with existing tests | Plan the change, implement it, and have a reviewer check behavior. |
| Fix several unrelated bugs | Propose independent steps with separate file scopes so they can run in parallel. |
| Use a local model for implementation and Claude for review | Assign different providers to the worker and reviewer roles. |

For a small edit you can finish in your current agent session, the extra workflow
may be unnecessary.

Horde currently serves one user through the CLI and MCP; it has no web dashboard.
It is an early release. It coordinates work, but the agents can still make
mistakes or fail to finish. Review the resulting code before shipping it. The
[verification record](docs/verification.md) separates automated coverage from live
provider checks and lists the current limits.

## Quick start

You need macOS or Linux, Git, and a coding-agent CLI or model server. Native workers
also use [ripgrep](https://github.com/BurntSushi/ripgrep) for search. The repository
you give Horde must have an initial commit and a configured Git author identity.

### Let Claude Code set it up

Open Claude Code in your repository and paste this prompt:

```text
Set up Horde for this repository using the installation guide at
https://github.com/gethamster/horde/blob/main/docs/installing.md.

Check the Git prerequisites and install the signed release if needed, without
installing a boot service. Configure the planner, worker, and reviewer roles to
use my installed Claude Code CLI and its existing login. Add Horde to Claude
Code as a user-scoped stdio MCP server with the command `horde mcp`.

Start Horde and run a simulated task in this repository to verify scheduling
without a model call. Show me the result, explain any remaining setup steps,
and give me a prompt for submitting my first real task through Horde.
```

Once the MCP connection is available, try:

```text
Use Horde to add CSV export with tests in this repository. Plan the work,
implement it, and review the combined result. Follow the task and show me
its result branch, the checks that ran, and anything that still needs attention.
```

Replace the CSV example with your task. See [Claude Code setup](#use-with-claude-code)
for the MCP command and provider options.

### Set it up from your terminal

Install a signed release:

```sh
curl -fsSL https://horde.sh/install -o install.sh
sh install.sh
```

Choose which agent will do the work. For example, to use an installed, signed-in
Codex CLI for every step:

```sh
horde config provider add codex --use-for planner,worker,reviewer
```

For Claude Code, replace `codex` with `claude`. For a local model or API provider,
see [settings](#settings). Then start Horde and submit a task:

```sh
horde start
horde submit "Add CSV export with tests" --repo /path/to/repository
```

Submission returns a task ID. Use it to follow the work:

```sh
horde inspect TASK_ID
horde events TASK_ID
horde metrics TASK_ID
```

Horde leaves your original checkout on its current branch. It puts the combined
result on `horde/TASK_ID`; `git worktree list` shows where to inspect it. Pushing a
branch or opening a PR requires [delivery configuration](docs/delivery.md).

To try scheduling without calling a model, submit a simulated task instead:

```sh
horde submit "Exercise the runtime" --repo /path/to/repository --template simulated
```

Stop the local service with `horde stop`. See [installation](docs/installing.md)
for service setup and updates, or [build from source](#build-from-source).

## Settings

Horde can use your existing CLI login or a provider API key. You can choose a
provider for each role:

| Executor | What you need |
| --- | --- |
| Codex | Installed Codex CLI with login or configured API access |
| Claude Code | Installed Claude CLI with login or configured API access |
| Native | An OpenAI-compatible endpoint with a model catalog and tool calling |

`horde config provider add` opens the provider setup prompts. The default
configuration uses Tuara for planning, implementation, and review; choose your
provider before starting a real task.

For a local model, put this in your repository's `.horde.toml`:

```toml
[providers.default]
base_url = "http://127.0.0.1:8122/v1"
api_key_env = "LOCAL_MODEL_KEY"
model = "auto"
```

Set `LOCAL_MODEL_KEY` in the daemon environment or its private credential file.
`auto` selects the only model listed by the server; if there are several, Horde
asks you to choose one. `horde doctor --repo /path/to/repository` checks the
configuration and reports the resolved model without requesting a completion.

The [configuration guide](docs/configuration.md) covers credentials and per-role
settings. The [native provider guide](docs/native-providers.md) explains streaming,
sampling options, and tool-event diagnostics.

## Connect your agent

### Use with Claude Code

After installing Horde and choosing a worker provider, add Horde to Claude Code:

```sh
claude mcp add --transport stdio --scope user horde -- horde mcp
horde start
cd /path/to/repository
claude
```

The user scope makes the connection available across your projects. In Claude
Code, try a request such as:

> Use Horde to add CSV export with tests in this repository. Submit the task,
> follow its progress, and show me the result branch and any failing checks.

Claude manages the conversation and calls Horde's tools. Horde runs the worker
models selected in your configuration. To use Claude Code for the worker roles too:

```sh
horde config provider add claude --use-for planner,worker,reviewer
```

This uses the installed Claude CLI and its existing login. You can also keep
Claude as the caller while using a local model for the workers.

### Other MCP clients

An agent that supports stdio MCP can submit and inspect Horde tasks. Add this
server to its MCP configuration:

```json
{
  "mcpServers": {
    "horde": { "command": "horde", "args": ["mcp"] }
  }
}
```

This connection controls the local runtime. Horde gives its own workers a separate,
scoped connection. See [agent connections and coordination](docs/coordination.md)
for custom data directories and external worker setup.

### Agent skills

Install the repository's agent skills to teach your agent how to operate Horde:

```sh
npx skills add gethamster/horde
```

[![skills.sh](https://skills.sh/b/gethamster/horde)](https://skills.sh/gethamster/horde)

The three skills cover running Horde, writing templates, and working inside a task.
These are separate from the [runtime skills](docs/runtime-skills.md) you can assign
to workers for your own project.

## Connect worker machines

Create a fleet credential with `horde network key create` and supply it to workers
through `HORDE_ENROLLMENT_FILE` or `HORDE_ENROLLMENT_JSON`. Each worker generates
its own key, enrolls without SSH, and renews its certificate automatically. The
same startup flow works across containers, sandboxes, VMs, and individual machines.
See [automatic fleet enrollment](docs/networking.md#automatic-fleet-enrollment)
for controller setup and credential delivery.

## Documentation

| Guide | What it covers |
| --- | --- |
| [Installation and releases](docs/installing.md) | Install, update, and repair an older database. |
| [Configuration](docs/configuration.md) | Choose providers, manage credentials, and configure roles. |
| [Native providers](docs/native-providers.md) | Use local models, tune requests, and inspect tool events. |
| [Agent connections and coordination](docs/coordination.md) | Connect agents, manage file ownership, and recover interrupted work. |
| [Authoring templates](docs/templates.md) | Define reusable workflows and acceptance checks. |
| [Runtime skills](docs/runtime-skills.md) | Give workers pinned instructions and supporting files. |
| [Delegation](docs/delegation.md) | Split work into child tasks while keeping the original context. |
| [Application secrets and environments](docs/environments.md) | Run checks in disposable app environments. |
| [GitHub delivery](docs/delivery.md) | Configure PR creation, merging, and deployment checks. |
| [Runtime networking](docs/networking.md) | Connect your machines through direct networking or Tailscale. |
| [Runtime management](docs/runtime-management.md) | Manage execution hosts, capacity, and updates. |
| [Architecture](docs/architecture.md) | Understand the daemon and its persistence model. |
| [Verification and limitations](docs/verification.md) | Review test coverage and known boundaries. |

The [documentation index](docs/README.md) also links to the [agent skills](skills/README.md)
and [contributing guide](CONTRIBUTING.md).

## Build from source

Install the Rust toolchain pinned by the repository, then:

```sh
git clone https://github.com/gethamster/horde.git
cd horde
cargo install --path . --locked
horde config init
```

## Contributing

Bug reports with a reproducible example and documentation fixes are welcome.
[Open an issue](https://github.com/gethamster/horde/issues) to report a problem or
discuss a change. Include the Horde version and relevant errors, with credentials
and private repository content removed.

For code changes, run:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for development expectations and
[CHANGELOG.md](CHANGELOG.md) for release history. Horde is licensed under
[Apache-2.0](LICENSE).
