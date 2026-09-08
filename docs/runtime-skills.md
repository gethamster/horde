# Runtime skills

Horde can load a skill into a worker's prompt and distribute its files to child
tasks. A skill is a directory containing `SKILL.md` and any references, scripts, or
assets it needs. Adding a skill to an editor's skill directory alone does not
activate it in Horde. Configure it as a named skill, then select it for a step or
read it through the worker tools.

## Configure and select

Add directories to the `[skills]` table in the repository's `.horde.toml` or your
Horde user configuration. Relative paths resolve against the submitted repository.
Use an absolute path for a skill stored outside that repository.

```toml
[skills]
report = ".agents/skills/report"
writer = "/home/me/shared-skills/writer"
```

Submission captures every configured directory into the task's available catalog.
Each bundle is addressed by the SHA-256 hash of its file contents, relative paths,
and executable flags. Subsequent source edits affect new tasks. Running tasks,
retries, and their children keep the captured version even if the source directory
is removed. A missing or invalid configured directory rejects submission.

Select skill names in a workflow step:

```toml
[[steps]]
id = "write_report"
skills = ["report"]
instructions = "Write the report using the supplied observations."
tools = ["read_file", "write_file", "command"]
scope = ["report.md"]
```

For agent steps, Horde inserts the selected `SKILL.md` instructions into the
initial prompt used by both native and CLI executors. It includes the task's
available skill names and hashes, so a planner can select skills in `propose_steps`.
Unknown names fail workflow validation. Skills selected on a nested template
invocation propagate to its agent steps. Command and environment steps execute
their configured commands; a skill does not rewrite those commands.

Workers can call `list_skills` to inspect the pinned catalog. `read_skill` reads
`SKILL.md` by default, or a resource path inside that bundle:

```json
{"name":"report","path":"references/format.md"}
```

Reads return bounded byte pages, a `next_offset`, the file list, and a local base
directory. A page is encoded as UTF-8 when possible and as hex otherwise. The
materialized directory contains the complete pinned files and sits outside the
Git checkout. Relative resources in the skill resolve against that directory.
Scripts retain their executable flag. Loading a skill never runs its scripts;
execution still requires the existing command permissions and harness restrictions.

## Distribute to a child

`delegate_task.skills` selects a subset of the parent's pinned catalog and loads
those skills into the child template's agent steps:

```json
{
  "id":"report-child",
  "objective":"Write the report from the verified measurements",
  "template":"local-implementation",
  "peer":"worker",
  "skills":["report"]
}
```

Omit `peer` for local delegation. Omitting `skills` inherits the available catalog,
with prompt selection left to the child template and planner. An empty array
passes no skills. Children cannot add skills outside their inherited catalog.
Further delegation follows the same rule.

Approved remote execution carries the bundles in the immutable assignment packet
over mTLS. The receiver verifies their hashes before binding the task. It does not
read skill paths from the sender's settings or substitute locally installed
versions. Assignment deduplication covers skill contents as well as the plan.
Both runtimes must support this skill contract. Schema version 3 adds task and
attempt skill bindings; an older binary rejects the upgraded database.

## Evidence and limits

`skill.pinned` records each task binding. `skill.loaded` records a selected skill
included in an attempt's initial prompt, once per name and attempt. `skill.read`
records worker resource reads. Events contain the skill name and hash, without
copying its instructions. These events show what Horde supplied, not proof that
a model followed every instruction.

Bundles accept regular files and directories, with no symlinks or escaping paths.
A task can pin at most 32 skills and 8 MiB of file contents. Each bundle supports
512 files; a file can be at most 1 MiB. `SKILL.md` must be nonempty UTF-8 and no
larger than 64 KiB. A step can load at most 256 KiB of skill instructions.
Put only intended skill resources in the configured directory. Credentials belong
in Horde's existing secret and provider configuration, never in a skill bundle.

Artifact bytes are verified before use. Materialized files are read-only and
checked against the bundle when loaded. Skill instructions do not expand task
scope, bypass claims, grant tools, or change acceptance criteria. Horde remains a
cooperative runtime; these checks are not a sandbox for malicious local code.
