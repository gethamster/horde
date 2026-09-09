# Horde documentation

Horde coordinates coding agents through a local daemon, CLI, and stdio MCP
bridge. These guides cover installing it, shaping work into reusable workflows,
running agents across machines, and understanding how durable state and ownership
keep their work coordinated.

Start with the [quick start](../README.md#quick-start) to run your first task,
then read [configuration](configuration.md) to configure providers and executors.
The [verification record](verification.md) describes tested behavior and current
limitations.

## Contents

| Guide | What it covers |
| --- | --- |
| [Installation and releases](installing.md) | Install, update, and repair an older database. |
| [Configuration](configuration.md) | Choose providers, manage credentials, and configure roles. |
| [Native providers](native-providers.md) | Use local models, tune requests, and inspect tool events. |
| [Agent connections and coordination](coordination.md) | Connect agents, manage file ownership, and recover interrupted work. |
| [Authoring templates](templates.md) | Define reusable workflows and acceptance checks. |
| [Runtime skills](runtime-skills.md) | Give workers pinned instructions and supporting files. |
| [Delegation](delegation.md) | Split work into child tasks while keeping the original context. |
| [Application secrets and environments](environments.md) | Run checks in disposable app environments. |
| [GitHub delivery](delivery.md) | Configure PR creation, merging, and deployment checks. |
| [Runtime networking](networking.md) | Connect your machines through direct networking or Tailscale. |
| [Runtime management](runtime-management.md) | Manage execution hosts, capacity, and updates. |
| [Architecture](architecture.md) | Understand the daemon and its persistence model. |
| [Verification and limitations](verification.md) | Review test coverage and known boundaries. |

For agent setup and skill installation, see [Agent skills](../skills/README.md).

For development setup and contribution guidelines, see [Contributing](../CONTRIBUTING.md).
