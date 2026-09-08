# Horde agent skills (source of truth)

Skills that teach a coding agent what Horde is, why it exists, and how to use it.

| Skill | For |
| --- | --- |
| `horde` | An agent using Horde: install, connect over MCP, submit and monitor tasks, answer questions, collect results, recover from a crash |
| `horde-templates` | Authoring the versioned TOML workflow templates Horde compiles into a task graph |
| `horde-worker` | An agent running *inside* a Horde task: claims, mail, questions, artifacts, integration |

These files are maintained here and mirrored to the public
[`asomervell/horde-skills`](https://github.com/asomervell/horde-skills)
repository, which is what skills.sh indexes and what
`npx skills add asomervell/horde-skills` installs. This README stays behind;
the public repository keeps its own.

```sh
scripts/sync_skills.sh /path/to/horde-skills-checkout
```

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
npx skills add ./ --list      # must find all three skills
```
