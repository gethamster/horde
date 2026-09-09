# Horde documentation

Horde coordinates coding agents through a local daemon, CLI, and stdio MCP
bridge. These guides cover installing it, shaping work into reusable workflows,
running agents across machines, and understanding how durable state and ownership
keep their work coordinated.

Start with the [quick start](../README.md#quick-start) to run your first task,
then read [settings](../README.md#settings) to configure providers and executors.
The [verification record](verification.md) describes tested behavior and current
limitations.

## Contents

| Guide | What it covers |
| --- | --- |
| [Installation and releases](installing.md) | Install and update Horde, verify signed downloads, and publish releases. |
| [Native providers](native-providers.md) | Configure local model discovery, role options, worker schemas, streaming, and bounded tool events. |
| [Runtime skills](runtime-skills.md) | Load pinned skill instructions and distribute their resources to local and remote workers. |
| [Authoring templates](templates.md) | Define workflows in TOML with inputs, dependencies, parallel steps, and outputs. |
| [Delegation](delegation.md) | Connect your own agent and delegate work through bounded task trees. |
| [Application secrets and environments](environments.md) | Share application configuration and run disposable process or Compose environments. |
| [Runtime management](runtime-management.md) | Configure execution hosts, concurrency, capacity, provisioning, and updates. |
| [Runtime networking](networking.md) | Connect machines directly or through Tailscale with authenticated enrollment. |
| [Architecture](architecture.md) | Understand scheduling, persistence, ownership, recovery, and the CLI/MCP interfaces. |
| [Verification and limitations](verification.md) | Review automated tests, live checks, and known boundaries. |

For agent setup and skill installation, see [Agent skills](../skills/README.md).

For development setup and contribution guidelines, see [Contributing](../CONTRIBUTING.md).
