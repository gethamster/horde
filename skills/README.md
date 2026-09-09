# Horde agent skills (source of truth)

Skills that teach a coding agent what Horde is, why it exists, and how to use it.

For repository setup with automatic delegation, use
`horde init --agent codex --delegate always` or
`horde init --agent claude --delegate always`. The command installs the bundled repository skills, MCP configuration, and a
managed repository instruction block.
See [repository setup](../docs/installing.md#make-horde-the-default-in-a-repository).

| Skill | For |
| --- | --- |
| `horde` | An agent using Horde: install, connect over MCP, submit and monitor tasks, answer questions, collect results, recover from a crash |
| `horde-templates` | Authoring the versioned TOML workflow templates Horde compiles into a task graph |
| `horde-worker` | An agent running *inside* a Horde task: claims, mail, questions, artifacts, integration |
| `horde-setup` | Provider, controller, and worker readiness |
| `horde-discovery` | Runtime and model names resolved against reported capabilities |
| `horde-model-selection` | Model choices matched to the task and available checks |
| `horde-planning` | Bounded tasks and execution choices from permitted models |
| `horde-delegation` | Execution contracts, retry identities, and pinned skills |
| `horde-review` | Result checks and persistent guidance review |
| `horde-sdlc` | Carrying intent, specifications, plans, verification evidence, and review decisions through an AI-native SDLC |

These skills are maintained and distributed from the `skills/` folder in
[gethamster/horde](https://github.com/gethamster/horde). Install the skills:

```sh
npx skills add gethamster/horde
```

This installs the skills only. It does not execute `horde init` or enable the
always-delegate policy, and is not needed when you use `horde init`.

Or install one:

```sh
npx skills add gethamster/horde --skill horde
```

[View on skills.sh](https://skills.sh/gethamster/horde). The root
`skills.sh.json` groups the skills on that page. Skills.sh discovers repositories
through CLI installation telemetry; there is no separate mirror to synchronize.

## Maintaining them

When a pull request changes the CLI surface, an operation's arguments, a
configuration key, or a default, update the matching reference file in the same
pull request. A skill describing a version of Horde that no longer exists is worse
than no skill.

- Every command, operation, argument, environment variable, and path in these
  files must exist in this repository. Check `src/protocol.rs` for operations and
  their schemas, `src/main.rs` for the CLI, `src/config.rs` for settings defaults,
  `src/template.rs` for template fields, and `src/branding.rs` for paths and
  environment variables.
- Keep the honest limits in. The value of these skills is that an agent reading
  them does not overstate what Horde guarantees.
- Frontmatter descriptions must be valid YAML on one line. A bare `: ` inside an
  unquoted description breaks parsing and the skill is silently skipped.
- After editing frontmatter, confirm discovery still works:

```sh
npx skills add ./ --list      # must include the skills listed above
```

## Skills used by workers

Repository skills help the caller operate Horde. The daemon separately loads its
default runtime skill pack and pins the selected resources for each task. Managed
workers read the selected instructions progressively with `read_skill`.

Horde can also load and distribute additional skills during task execution. Configure named
directories in `[skills]`, select names in workflow steps, and pass a subset through
`delegate_task.skills` when assigning child work. See [runtime skills](../docs/runtime-skills.md).
