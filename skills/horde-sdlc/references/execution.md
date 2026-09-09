# Execution and review

Use the live Horde tool schemas for exact arguments. The caller owns phase
transitions and external authorization; workers perform their bounded assignment.
An assigned worker should return its handoff to the caller rather than independently
start the rest of the lifecycle.

## Prepare and submit

Inspect existing templates before choosing one. `local-implementation` runs
planning, implementation, and a reviewer that may repair code automatically. It
does not pause after the plan. For an intent, design, or planning-only assignment,
author a project template containing only that work. When a required decision is
pending, omit downstream implementation and delivery from the submitted graph.
After the decision, the caller can submit a separate task or append only the
newly authorized work through `add_steps`.

Project templates live in `.horde/templates/`. For an agent step that needs this
guidance, select `skills = ["horde-sdlc"]` from the available catalog. Add other
applicable skills explicitly; this selection changes planner defaults. The skill
does not install or activate a template on its own.

Validate the selected template with its real objective:

```sh
horde validate my-sdlc-template --repo /path/to/repository --objective "Implement the accepted export specification"
```

Check the returned `skill_catalog`, including resolver errors, and correct
warnings about ignored fields. Do not invent `kind = "approval"`, a phase field,
or an approval flag. Existing step kinds are `agent`, `command`, `delivery`,
`environment`, and `simulated`.

Submit through `submit_task` with `objective`, `repo`, `template`, a stable
`request_id`, and relevant `context` records. Each context record can carry
`kind`, `content`, and `provenance`. Include the original constraints, required
criteria, source revisions, and existing authorization. Pin the actual source
content in context or artifacts and give workers references they can resolve.
Page supporting context with `read_context`; do not fill mandatory context with
whole transcripts. Each context record is limited to 64 KiB, and mandatory
context is bounded to 256 KiB. There is no top-level `skills` or arbitrary
template-input argument on `submit_task`; its `objective` supplies `{{task}}`.

Verify artifact and command-file availability in the task workspace. Integrated
worktrees do not copy dirty or untracked files from the caller's checkout and may
start from a configured remote base. A path in the objective does not copy its
contents. Have an earlier authorized step materialize pinned inputs when needed,
or use the project's committed source workflow. Do not push files merely to make
them available unless publishing them is already authorized.

Retry an uncertain submission with its original request ID and unchanged inputs.
For delegation, preserve a stable child ID, inherited context, permitted executor
pool, and pinned skills. Check available tools rather than assuming every caller
has remote execution or third-party connectors.

## Verify and review

Translate accepted work into explicit dependencies and non-overlapping write
scopes where possible. Use plain command steps for deterministic repository
checks, run against the integrated workspace. Apply the project's test commands;
do not substitute a generic test command for required integration or behavior
checks. Use existing eval commands separately when agent configuration changes.

Keep repair separate from the final review. A review-only assignment reports
defects without editing. If a worker repairs despite that scope, preserve and
report the unexpected commit; do not treat it as authorized or advance it toward
delivery. For an authorized repair, follow with fresh combined checks and another
final review. Apply `REVIEW.md` when present and check the diff against the
pinned specification and plan. Record substantive findings with their evidence.
Restrict reviewer tools where supported, but do not claim a prompt or empty write
scope creates a sandbox, especially when shell commands remain available.

Bound repair attempts. Use explicit failure branches or caller-added steps after
inspecting a failed check. `propose_steps` is available to an active planner;
`add_steps` is the caller's append operation. Validate dependencies and output
references instead of inventing a cycle or assuming a failed task repairs itself.

An agent's `accepted` result and typed outputs do not establish that each
criterion passed. Read command exit results, integration evidence, and findings
before describing the result as verified. Follow task progress with `inspect`,
`events`, and `summary`; retain task IDs in the evidence report.

## Decisions and changed inputs

For an unresolved decision during an invocation, use `request_question` with the
question, evidence, and recommendation. Set `human_only` when policy requires a
human answer. The immediate caller answers or escalates the same envelope;
external `human: true` is an attestation, not independent identity verification.
The question holds its worker, not every unrelated branch. Follow the tool's
instruction to finish that invocation with `accepted=false`. When an answer
arrives, read its decision and scope: a rejection resolves the question but does
not authorize the requested work.

Use `update_context` for a caller correction rather than silently changing the
meaning of a pinned source. Authoritative context updates and answers advance the
family version and can invalidate prior results. Inspect the resulting state,
append fresh verification when required, and resume through the existing recovery
flow. Never replay completed implementation or delivery merely to refresh context.

When approval applies to a specific artifact or commit, compare the current
revision with that record before proceeding. A relevant change requires another
decision unless the existing authorization explicitly covers it. Keep mandatory
organizational controls in the existing branch protections, managed harness
settings, or release system; this skill cannot enforce them across executors.

## Delivery and maintenance

Return a local branch and evidence unless PR publication is authorized. For an
authorized PR-only handoff, use an explicit delivery step with delivery enabled
and `merge = false`; inspect existing settings before submission. Do not select
`github-actions` blindly, because it includes Next.js-specific checks. Merge and
deployment require the authorization and external controls appropriate to them.
Record actual delivery status, including skipped delivery and its reason.

For a supplied incident, retain its stable identity and check existing task
receipts or the caller's incident mapping before creating another task. Diagnose
within the authorized scope, then return proposed intent for triage. Existing
monitoring integrations can submit through the same CLI/MCP path. Creating a
scheduled watcher or granting production actions is a separate user-directed
configuration change. A delivered fix should include the incident's regression
test or eval and preserve its connection to the original signal.
