# Runtime skills

Horde includes workflow guidance for setup, capability discovery, model selection,
planning, delegation, and review. Releases ship ordinary skill directories beside
the executable. Horde reads those files when a new task is submitted; adding,
removing, or editing skills does not require a new Rust binary. You can also
configure skill directories with their own references, scripts, and assets.

## Default workflow guidance

| Skill | Purpose |
| --- | --- |
| `horde-setup` | Check controller and worker readiness within the requested scope. |
| `horde-discovery` | Resolve runtime and model names against advertised capabilities. |
| `horde-model-selection` | Match task difficulty and verification needs to permitted models. |
| `horde-planning` | Choose bounded tasks and execution capabilities from permitted pools. |
| `horde-delegation` | Preserve execution contracts, retry identities, and skill pins. |
| `horde-review` | Check task results and review proposed persistent guidance changes. |

The caller can use `skill_inspect` with a repository to list the effective
catalog, or add `name` to read one skill and its baseline. Inside a task,
`list_skills` and `read_skill` expose only that task's pinned catalog. Every managed
agent invocation loads the pinned `horde-model-selection` instructions when that
skill is available, including invocations with an explicit step skill selection.
Selecting it explicitly does not load a second copy. Agent steps with role
`planner` and no explicit `skills` also load the pinned `horde-planning`
instructions automatically when available. An explicitly narrowed child catalog
never gains a skill from the receiver's local installation, including these defaults.

The model selection defaults favor strong reasoning for difficult planning and
ambiguous investigations, competent mid-tier models for bounded implementation,
and tiny or fast models for narrow work with decisive checks. They prefer
deterministic tools when sufficient and scale review effort to uncertainty and
failure cost. Workers report which models can execute; the calling agent needs
project guidance, verified provider documentation, or observed results to assess
their suitability. Horde does not assign quality tiers or infer price from model
names. A model's reasoning-effort setting and execution harness are separate
choices, subject to the executor's supported configuration.

For a request such as “think locally with Codex and Astra, then deliver on Apollo
using Claude, Codex, or GLM 5.3,” the skills guide the caller through
`runtime_capabilities` and `plan_execution`. The caller chooses among the real,
advertised capabilities in each allowed pool; the list does not require every
model to run. A selected capability stays paired with its runtime in the task's
execution contract. A root that delegates across role pools carries their approved
union while selecting its own thinking capability; delivery children narrow that
pool to their delivery choices. Role constraints remain in the task context.
Use a stable `request_id` for `submit_task` and stable child
`id` for `delegate_task` so retries do not create duplicate work.

## Install and update independently

Edit the canonical `skills/` directory, then install the complete pack:

```sh
horde skills install ./skills
horde skills list
horde runtime update apollo --skills
horde runtime inspect apollo
```

Your coding agent can use `skill_pack_install` with an absolute `path`, then
`runtime_skills_update` with `id` and a stable `request_id`. The controller captures
its current default pack when it accepts the request and sends those exact bytes
over the authenticated management connection. Retrying that request does not pick
up intervening edits. Wait for its operation to report `succeeded`; the result and
worker inventory include the installed pack's content hash and skill names.

A pack replaces the complete default catalog for that runtime. Removing a skill
folder before installation removes it from future submissions. Installation needs
no restart and preserves existing task pins. Project overrides remain separate;
they are captured into task assignments rather than copied into every worker's
default pack. Each runtime stores installed packs under its data directory and
prefers the selected pack to executable-adjacent defaults, including after a binary
upgrade. Development builds inside this checkout's `target/` directory fall back
to this repository's `skills/` files when no pack is installed.

## Define automatic injection in files

A skill can include an optional `horde.toml` sidecar. For guidance needed in every
agent invocation:

```toml
[injection]
agent = true
```

For guidance needed only by a planner without an explicit step selection:

```toml
[injection]
roles = ["planner"]
when_no_explicit_skills = true
```

These fields control automatic loading from the task's pinned catalog. They do
not add a skill to a narrowed child's catalog or inject instructions into command
steps. Explicit selections retain their order and never load a second copy.
The sidecar is pinned with the skill's other files, so its edits follow the same
version rules as instruction edits. Without a sidecar, a skill remains available
for explicit selection or `read_skill`.

## Review project overrides

Shipped and configured bundles form the baseline. An approved override has its
own version and does not change those source files. Overrides are scoped to the
repository's canonical path in the controller's SQLite store. They are local to
that controller; submitted and delegated tasks carry the effective bundle bytes.
No skill operation writes into the repository or edits user configuration.

The caller can inspect and propose a change through these tools:

```text
skill_inspect {"repo":"/work/project","name":"horde-planning"}
skill_propose {"repo":"/work/project","name":"horde-planning","content":"Complete replacement SKILL.md instructions","expected_hash":"HASH_FROM_INSPECTION","reason":"Explain the project-specific improvement"}
```

`skill_propose` saves a draft and returns its ID, instruction diff, and changed
resource hashes. It does not alter effective guidance. Review the actual content
and scope with the user, then apply that proposal after explicit acceptance:

```text
skill_apply {"repo":"/work/project","proposal_id":"REVIEWED_PROPOSAL_ID","accepted":true}
```

Apply checks the proposal's prior revision, effective hash, and baseline hash in
one transaction. A changed baseline or intervening edit rejects the stale
proposal. Retrying an applied proposal returns its original receipt without
creating another revision. Worker credentials cannot use the administrative
skill tools; workers can send suggested changes to their caller.

`skill_history` accepts `repo` and `name`, with optional `limit` and
`before_revision` for pagination. `skill_rollback` accepts a historical revision
and creates another proposal for review and acceptance. Revision `0` proposes a
reset to the current baseline. A later revision restores the complete saved
bundle, including its resource files and executable flags. The response lists
resource changes even when the instruction text is unchanged.

Overrides affect new submissions. Running tasks, retries, and their children
retain the versions already pinned to those tasks, including after rollback.
An approved override keeps its complete bundle until changed or reset, so edits
to a configured source directory do not silently replace its references.

## Configure and select

Add directories to the `[skills]` table in the repository's `.horde/horde.toml` or your
Horde user configuration. Relative paths resolve against the submitted repository.
Use an absolute path for a skill stored outside that repository.

```toml
[skills]
report = ".agents/skills/report"
writer = "/home/me/shared-skills/writer"
```

Submission captures every configured directory alongside the installed default skills
and applies approved project overrides to form the task's available catalog.
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

For agent steps, Horde inserts the selected `SKILL.md` instructions and the
available automatic defaults described above into the initial prompt used by
both native and CLI executors. It includes the task's
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
with prompt selection left to the child template, planner, and automatic defaults.
An empty array
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
A task can pin up to 64 skills (up to 32 explicitly configured directories), with at most
8 MiB of file contents. Each bundle supports
512 files; a file can be at most 1 MiB. `SKILL.md` must be nonempty UTF-8 and no
larger than 64 KiB. A step can load at most 256 KiB of skill instructions.
Put only intended skill resources in the configured directory. Credentials belong
in Horde's existing secret and provider configuration, never in a skill bundle.

Artifact bytes are verified before use. Materialized files are read-only and
checked against the bundle when loaded. Skill instructions do not expand task
scope, bypass claims, grant tools, or change acceptance criteria. Horde remains a
cooperative runtime; these checks are not a sandbox for malicious local code.
