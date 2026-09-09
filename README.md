# Horde

Running several coding agents means keeping their work in sync. Horde plans tasks,
gives workers separate Git worktrees, and combines their changes for review.

Built by [Hamster Research](https://tryhamster.com/research). Open source on macOS
and Linux, with support for Claude Code, Codex, and local or hosted
OpenAI-compatible models.

For Horde delivery across your team, with infrastructure and model providers
managed for you, [talk to Hamster](https://tryhamster.com).

[Quick start](#quick-start) · [Documentation](docs/README.md) · [Website](https://horde.sh) · [Issues](https://github.com/gethamster/horde/issues) · [Apache-2.0](LICENSE)

## When to use Horde

| Task | How Horde helps |
| --- | --- |
| Build an API and its UI | Run separate workers, then review the combined changes. |
| Refactor a tested module | Give implementation and review separate steps. |
| Fix unrelated bugs | Run independent steps in parallel with separate file scopes. |
| Mix local and hosted models | Choose a provider for each worker role. |

For a small edit, your current agent session may be enough. Horde is an early
release for a single user; it has no web dashboard. Review generated code before
shipping. See [verification and limitations](docs/verification.md).

## Quick start

You need Git and a repository with an initial commit and author identity. Install
Claude Code and sign in, then open it in your repository and paste:

```text
Set up Horde using https://github.com/gethamster/horde/blob/main/docs/installing.md.
Install the signed release if needed, without a boot service. Use my existing
Claude Code login for the planner, worker, and reviewer roles. Add a user-scoped
stdio MCP server named horde with the command `horde mcp`.
Start Horde and verify setup with a simulated task in this repository.
Report the result and any remaining setup steps.
```

Once connected, give Claude a task:

```text
Use Horde to add CSV export with tests in this repository. Follow the task
through review, then show me the result branch, checks, and unresolved problems.
```

<details>
<summary>Set up from your terminal</summary>

```sh
curl -fsSL https://horde.sh/install -o install.sh
sh install.sh
horde config provider add claude --use-for planner,worker,reviewer
claude mcp add --transport stdio --scope user horde -- horde mcp
horde start
cd /path/to/repository
claude
```

You can also submit and follow tasks directly:

```sh
horde submit "Add CSV export with tests" --repo /path/to/repository
horde inspect TASK_ID
horde events TASK_ID
horde metrics TASK_ID
horde call list_workers '{"task":"TASK_ID"}'
horde steer TASK_ID "Prefer the streaming parser; skip the CLI flag"
horde steer TASK_ID "Only you: re-check the parser" --worker WORKER_ID
```

</details>

Results land on `horde/TASK_ID`; `git worktree list` shows their location. Your
original checkout stays on its branch. [Delivery](docs/delivery.md) can push results
and open PRs. Use `horde stop` to stop the service.

## Settings

Claude can manage the conversation while other models do the work. Use
`horde config provider add` to choose providers, or configure a
[local model](docs/native-providers.md#provider-request-options). The default
provider is Tuara; select your provider before running a real task.

Native workers need an endpoint with a model catalog and tool calling, plus
[ripgrep](https://github.com/BurntSushi/ripgrep) for search. The
[configuration guide](docs/configuration.md) covers credentials and role settings.

## Connect your agent

Any stdio MCP client can connect using command `horde` and arguments `["mcp"]`.
See [agent connections](docs/coordination.md) for the JSON configuration, custom
data directories, and external workers.

Install the agent skills for operating Horde and writing templates:

```sh
npx skills add gethamster/horde
```

[![skills.sh](https://skills.sh/b/gethamster/horde)](https://skills.sh/gethamster/horde)

You can also assign your own [runtime skills](docs/runtime-skills.md) to workers.

## Connect worker machines

Tell your coding agent how you want to divide the work:

```text
Use Apollo for delivery with Claude, Codex, and GLM 5.3 available to choose from.
Keep the thinking on this machine using Codex and Astra. Follow the work through
implementation and tests, then bring me the result for review.
```

Horde ships file-based skills for setup, discovery, model selection, planning, delegation, and review. The
agent reads the workers' reported capabilities, resolves your machine and model
names, and chooses a model for each task within the pools you specified. The
names in this example must match models and providers available on your machines.

The agent can arrange enrollment and provider configuration using its available
machine or platform access. Missing access or credentials are reported directly.
Workers retain their own provider credentials, and task choices do not rewrite
their configuration. Containers, sandboxes, and VMs use the same enrollment flow
through their platform's secret delivery mechanism.

You can install edited skill files with `horde skills install ./skills` and send
the pack to a worker with `horde runtime update apollo --skills`, without replacing
the binary. You can discuss changes to those skills with your agent. It proposes a project
override for review and saves it when you agree; running tasks keep their pinned
instructions. See [agent-led delegation](docs/delegation.md#let-your-agent-arrange-the-work)
and [runtime skills](docs/runtime-skills.md). Terminal setup remains documented in
[fleet enrollment](docs/networking.md#automatic-fleet-enrollment).

## Documentation

| Guide | What it covers |
| --- | --- |
| [Installation and releases](docs/installing.md) | Install, update, and repair databases. |
| [Configuration](docs/configuration.md) | Providers, credentials, and roles. |
| [Native providers](docs/native-providers.md) | Local models, request options, and tool events. |
| [Agent connections and coordination](docs/coordination.md) | MCP, ownership, and recovery. |
| [Authoring templates](docs/templates.md) | Workflows and acceptance checks. |
| [Runtime skills](docs/runtime-skills.md) | Pinned instructions and resources. |
| [Delegation](docs/delegation.md) | Child tasks and inherited context. |
| [Application secrets and environments](docs/environments.md) | Disposable app environments. |
| [GitHub delivery](docs/delivery.md) | PRs, merging, and deployment checks. |
| [Runtime networking](docs/networking.md) | Direct connections and Tailscale. |
| [Runtime management](docs/runtime-management.md) | Hosts, capacity, and updates. |
| [Architecture](docs/architecture.md) | Scheduling and persistence. |
| [Verification and limitations](docs/verification.md) | Test coverage and boundaries. |

## Contributing

[Report a bug or propose a change](https://github.com/gethamster/horde/issues).
Include a reproducible example and your Horde version; remove secrets from logs.
See [CONTRIBUTING.md](CONTRIBUTING.md) for checks and development details.

To build from source with the repository's pinned Rust toolchain:

```sh
git clone https://github.com/gethamster/horde.git
cd horde
cargo install --path . --locked
horde config init
```

[Changelog](CHANGELOG.md) · [Apache-2.0 license](LICENSE) · [Hamster Research](https://tryhamster.com/research)
