# Horde

Horde connects the agent you already use to workers running other models and
harnesses. You can plan with a frontier model and delegate implementation to workers
using open-weight models or another subscription. Horde manages the handoffs and combines results,
so you can keep working in one conversation.

Built by [Hamster Research](https://tryhamster.com/research). Open source on macOS
and Linux, with support for Claude Code, Codex, and local or hosted
OpenAI-compatible models.

For Horde delivery across your team, with infrastructure and model providers
managed for you, [talk to Hamster](https://tryhamster.com).

[What does Horde do?](#what-does-horde-do) · [Quick start](#quick-start) · [Repo setup](#make-horde-the-repo-default) · [Example prompts](docs/example-prompts.md) · [Documentation](docs/README.md)

## What does Horde do?

When your coding subscription runs out before a task is finished, you can pay for
extra usage or buy another subscription. Working across several harnesses also
means passing context between agents and combining their changes yourself.

Tell your existing agent what you want built, and it uses Horde to assign the
work and follow each worker's progress. The agent returns the result in your
conversation or asks you for a decision when a worker needs one.

Assign planning and review to a frontier model, and let implementation workers
use open-weight models or another coding subscription. Horde manages file ownership
and combines the changes for testing. You can change each role's model and harness
for new tasks without changing your chat agent or rewriting the workflow.

Enable [delegation in your repo](#make-horde-the-repo-default) once, and your agent
will use Horde without being asked each time. The daemon continues running the
workers after you close the conversation.

After repo setup and provider selection, you can simply ask:

```text
Add a CSV download button to the orders page. Export only orders matching
the current filters, and handle an empty export.
Test and review the change. Show me the result before opening a PR.
```

Your agent can set up this workflow:

1. The frontier model inspects the app and plans the endpoint and button.
2. Two open-weight workers implement those parts in parallel, with separate file
   scopes and Git worktrees so each worker owns the files it edits.
3. Horde combines their commits and runs the configured checks. The frontier
   model reviews the result, including whether the endpoint and button work together.
4. Your agent returns the checks and the `horde/TASK_ID` result branch for you to
   inspect. Your original checkout stays on its branch.

Because the implementation workers use a different provider, their model calls
do not consume your planning and review subscription. Your total cost depends on
the models you choose and the work they need to do, including retries. Horde
reports the usage providers make available; the
[evaluation record](docs/verification.md#evaluation) describes what has been measured.

You can also [run workers on other machines](#connect-worker-machines) to move
builds and test processes off your laptop. Your agent can check progress using the
task ID; independent work can continue while a worker waits for a decision.
[GitHub delivery](docs/delivery.md) can push the result and open a PR when you enable it.

Use the [quick start](#quick-start) to connect your agent, or
[make Horde the repo default](#make-horde-the-repo-default) so you do not need to
ask for delegation each time. See [example prompts](docs/example-prompts.md) for
bugs, refactoring, and other tasks.

Horde is an early release for a single user. Review generated code before
shipping; see [verification and limitations](docs/verification.md).

## Quick start

1. Install Horde with the command from [horde.sh](https://horde.sh):

   ```sh
   curl -fsSL https://horde.sh/install | bash
   ```

2. Ask your existing MCP-capable agent to set up the repository:

   ```text
   Set up Horde for this repository using `horde mcp`. Make Horde the default
   for repository work and save that rule in the instructions you load.
   ```

   Complete any setup steps the agent reports, then reload its instructions.
   See [repo setup](#make-horde-the-repo-default) for details.

3. Give your agent a normal work request:

   ```text
   Deliver CSV export for the filtered orders table, with tests and review.
   ```

   From then on, your agent uses Horde for repository work without you having to
   mention it. See [example prompts](docs/example-prompts.md) for more tasks to try.

<details>
<summary>Submit, follow, and steer tasks from your terminal</summary>

```sh
horde submit "Add CSV export with tests" --repo /path/to/repository
horde watch TASK_ID        # NDJSON events until the task ends; exit 0/1/2 by outcome
horde inspect TASK_ID
horde events TASK_ID
horde metrics TASK_ID
horde summary TASK_ID      # status, step outcomes, integrated head, PR URL or why delivery was skipped
horde call list_workers '{"task":"TASK_ID"}'
horde steer TASK_ID "Prefer the streaming parser; skip the CLI flag"
horde steer TASK_ID "Only you: re-check the parser" --worker WORKER_ID
```

A `[notify]` table in the configuration pushes the same milestones to a webhook
or local command. See [progress and notifications](docs/progress.md).

</details>

Results land on `horde/TASK_ID`; `git worktree list` shows their location. Your
original checkout stays on its branch. [Delivery](docs/delivery.md) can push results
and open PRs. Use `horde stop` to stop the service.

## Settings

Choose a provider for each role. For example, if you have both Claude Code and
Codex installed and signed in, use Claude for planning and review and Codex for
implementation:

```sh
horde config provider add claude --use-for planner,reviewer
horde config provider add codex --use-for worker
```

To use open-weight models for implementation, configure a
[local or hosted model](docs/native-providers.md#provider-request-options) and
assign its named provider to the `worker` role. Your chat agent can stay the same
when you change worker providers. The default provider is Tuara; select your
providers before submitting real work. Use `.horde/horde.toml` for repository
settings. Horde pins the effective settings when a task is submitted.

Native workers need an endpoint with a model catalog and tool calling, plus
[ripgrep](https://github.com/BurntSushi/ripgrep) for search. The
[configuration guide](docs/configuration.md) covers credentials and role settings.

## Make Horde the repo default

To make your agent delegate repository changes to Horde, run this in the target
repository after installing Horde:

```sh
horde init --agent codex --delegate always
# Or: horde init --agent claude --delegate always
```

This installs the bundled skills and adds repository instructions and MCP
configuration. It checks setup without calling a model and starts the daemon when
the local prerequisites are present. Follow any reported setup steps, then reload
your agent. See [repository setup](docs/installing.md#make-horde-the-default-in-a-repository).

## Connect your agent

Your existing agent or bot can operate Horde through its CLI or stdio MCP bridge.
Claude Code and Codex can connect directly. For Grokbot, ChatGPT, or another chat
interface, use an integration that can reach and call the Horde instance. The
conversation stays in that interface; Horde manages the workers behind it.

Any stdio MCP client can connect using command `horde` and arguments `["mcp"]`.
See [agent connections](docs/coordination.md) for the JSON configuration, custom
data directories, and external workers.

Install the agent skills for operating Horde and writing templates:

```sh
npx skills add gethamster/horde
```

`npx skills add` installs skills only; it does not run `horde init` or enable
automatic delegation. You do not need to run both commands for repository setup.

[![skills.sh](https://skills.sh/b/gethamster/horde)](https://skills.sh/gethamster/horde)

You can also assign your own [runtime skills](docs/runtime-skills.md) to workers.

## Connect worker machines

Running several agents, builds, and test suites on one laptop can make it
unresponsive. Assign workers to other machines so your laptop has more CPU and
memory available for your editor and chat. Your existing agent can submit and
monitor the remote work through Horde.

Tell your coding agent how you want to divide the work:

```text
Use Apollo for delivery with Claude, Codex, and GLM 5.3 available to choose from.
Keep the thinking on this machine using Codex and Astra. Follow the work through
implementation and tests, then bring me the result for review.
```

Horde ships file-based skills for setup, discovery, model selection, planning,
delegation, and review. The agent reads the workers' reported capabilities,
resolves your machine and model names, and chooses a model for each task within
the pools you specified. The names in this example must match models and
providers available on your machines.

The agent can arrange enrollment and provider configuration using its available
machine or platform access. Missing access or credentials are reported directly.
Workers retain their own provider credentials, and task choices do not rewrite
their configuration. Containers, sandboxes, and VMs use the same enrollment flow
through their platform's secret delivery mechanism.

You can install edited skill files with `horde skills install ./skills` and send
the pack to a worker with `horde runtime update apollo --skills`, without replacing
the binary. You can discuss changes to those skills with your agent. It proposes
a project override for review and saves it when you agree; running tasks keep
their pinned instructions. See [agent-led delegation](docs/delegation.md#let-your-agent-arrange-the-work)
and [runtime skills](docs/runtime-skills.md). Terminal setup remains documented in
[fleet enrollment](docs/networking.md#automatic-fleet-enrollment).

## Documentation

| Guide | What it covers |
| --- | --- |
| [Repository setup](docs/installing.md#make-horde-the-default-in-a-repository) | Make delegation the default with `horde init`. |
| [Example prompts](docs/example-prompts.md) | Features, fixes, parallel work, and task follow-up. |
| [Installation and releases](docs/installing.md) | Install, update, and repair databases. |
| [Configuration](docs/configuration.md) | Providers, credentials, and roles. |
| [Native providers](docs/native-providers.md) | Local models, request options, and tool events. |
| [Agent connections and coordination](docs/coordination.md) | MCP, ownership, and recovery. |
| [Authoring templates](docs/templates.md) | Workflows and acceptance checks. |
| [Runtime skills](docs/runtime-skills.md) | Pinned instructions and resources. |
| [Delegation](docs/delegation.md) | Child tasks and inherited context. |
| [Application secrets and environments](docs/environments.md) | Disposable app environments. |
| [GitHub delivery](docs/delivery.md) | PRs, merging, and deployment checks. |
| [Progress and notifications](docs/progress.md) | Follow task events, read summaries, and configure notifications. |
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
horde skills install ./skills
```

[Website](https://horde.sh) · [Changelog](CHANGELOG.md) · [Apache-2.0 license](LICENSE) · [Hamster Research](https://tryhamster.com/research)
