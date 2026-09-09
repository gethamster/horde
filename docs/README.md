# Horde documentation

Your existing agent can use Horde to assign coding work to other models and
machines while you stay in one conversation. Start by connecting your agent and
choosing the providers that will plan, implement, and review changes.

Start with [What does Horde do?](../README.md#what-does-horde-do) for a worked
example, then use the [quick start](../README.md#quick-start) to run your first task.
[Repository setup](installing.md#make-horde-the-default-in-a-repository) makes
delegation the default with `horde init`; [example prompts](example-prompts.md)
cover other kinds of work. The [verification record](verification.md) describes
tested behavior and current limitations.

## Contents

| Guide | What it covers |
| --- | --- |
| [What does Horde do?](../README.md#what-does-horde-do) | Keep your existing agent while Horde coordinates work across providers. |
| [Repository setup](installing.md#make-horde-the-default-in-a-repository) | Install repo instructions, skills, and MCP configuration with `horde init`. |
| [Example prompts](example-prompts.md) | Copy prompts for features, fixes, parallel work, and task follow-up. |
| [Installation and releases](installing.md) | Install, update, and repair an older database. |
| [Configuration](configuration.md) | Choose providers, manage credentials, and configure roles. |
| [Native providers](native-providers.md) | Use local models, tune requests, and inspect tool events. |
| [Agent connections and coordination](coordination.md) | Connect agents, manage file ownership, and recover interrupted work. |
| [Authoring templates](templates.md) | Define reusable workflows and acceptance checks. |
| [Runtime skills](runtime-skills.md) | Give workers pinned instructions and supporting files. |
| [Delegation](delegation.md) | Split work into child tasks while keeping the original context. |
| [Application secrets and environments](environments.md) | Run checks in disposable app environments. |
| [GitHub delivery](delivery.md) | Configure PR creation, merging, and deployment checks. |
| [Progress and notifications](progress.md) | Stream task events with `horde watch`, read the terminal summary, and push milestones to a webhook or command. |
| [Runtime networking](networking.md) | Move worker execution to other machines through direct networking or Tailscale. |
| [Runtime management](runtime-management.md) | Manage execution hosts, capacity, and updates. |
| [Architecture](architecture.md) | Understand the daemon and its persistence model. |
| [Verification and limitations](verification.md) | Review test coverage and known boundaries. |

For agent setup and skill installation, see [Agent skills](../skills/README.md).

For development setup and contribution guidelines, see [Contributing](../CONTRIBUTING.md).
