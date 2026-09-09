# Example prompts

Paste these prompts into Claude Code, Codex, or another coding agent connected to
Horde through MCP. Your agent submits work and follows it through Horde's tools.
Replace bracketed placeholders before sending. If Horde is not connected yet,
start with the [quick start](../README.md#quick-start). To make delegation the
repository default, use [repository setup](installing.md#make-horde-the-default-in-a-repository).

With repository delegation enabled, describe the work normally; your agent uses
Horde without a repeated “Use Horde” instruction. The examples include that phrase
so they also work in sessions where delegation is optional. For a small edit in
those sessions, your current agent may be enough.

## Write a task Horde can verify

Include the repository, the behavior you want, and examples of success. Name any
constraints, the checks to run, and where work should stop. For example:

```text
Use Horde in [absolute repository path] to [desired outcome].
The change is complete when [observable acceptance criteria].
Preserve [existing behavior or interfaces] and keep changes within [scope].
Run [validation commands] against the integrated result and review the diff.
Keep delivery disabled. Return the task ID, result branch, checks that ran,
and any unresolved findings.
```

These are instructions to your agent. The agent must turn workflow requirements
into templates, validated steps, and configuration. A sentence requesting parallel
work or retries does not itself change Horde's task graph. The default
`local-implementation` template runs planning, implementation, and review; add
explicit command steps for checks the workflow must enforce. See
[authoring templates](templates.md).

## Keep frontier usage for planning and review

Replace the provider names with ones already configured in Horde. Your choice of
chat agent is separate from the providers assigned to worker roles.

```text
Configure Horde in this repository to use [frontier provider] for the planner
and reviewer roles, and [implementation provider] for the worker role.
Preserve the providers' existing credentials and connection settings.
Verify the effective provider and model for each role before submitting.
Then use Horde to implement [feature], with [acceptance criteria].
Run [validation commands] against the combined changes and review the result.
Keep delivery disabled. Return the result branch, check results, and reported
usage by role. Mark any unreported cost or subscription capacity as unknown.
```

Choose an open-weight provider or another coding subscription for implementation.
Configure repository role settings in `.horde/horde.toml` before submission
because Horde pins them to the task. See [configuration](configuration.md) for
provider setup.

## Build a feature

```text
Use Horde to add CSV export for the filtered orders table in this repository.
Export the rows matching the current filters, with columns for order ID, date,
status, and total. Exclude customer names and email addresses. An empty result
should produce a file containing only the header row.
Add tests for filtering, empty results, and escaping commas and quotes.
Run the project's relevant tests and build against the integrated result, then
review the change. Keep delivery disabled and return the result branch,
validation results, and unresolved findings.
```

## Fix a reproducible bug

```text
Use Horde to fix this bug in [absolute repository path]: [observed behavior].
Reproduce it with [steps or failing command]. The expected behavior is
[expected result]. Add a regression test that fails before the fix.
Keep the change focused and preserve existing public interfaces.
Run the regression test and related tests against the integrated result,
then review the diff. Keep delivery disabled. Return the root cause,
result branch, and check results.
```

## Split an API and UI feature across workers

```text
Use Horde to add saved searches in this repository. Users should be able to
name the current filters, list their saved searches, and apply or delete one.
Agree on the API request and response shapes before splitting implementation.
Give the API and UI workers separate file scopes. Assign shared types and
schema changes to one owner, and make dependent work wait for that contract.
Run independent work in parallel where the scopes and dependencies allow it.
Add API tests and a UI test for saving and applying a search. Verify the combined
result and review it. Keep delivery disabled. Return the task ID, result branch,
check results, and remaining issues.
```

Horde allocates separate coding worktrees and serializes integration. Workers
still need claims for shared files. See [coordination](coordination.md) for file
ownership and [delegation](delegation.md) for larger assignments using child tasks.

## Refactor without changing behavior

```text
Use Horde to separate parsing from output formatting in [module path].
Preserve the public API, error messages, and output format. Add characterization
tests for behavior that is not already covered before changing the implementation.
Keep unrelated cleanup out of scope. Run the relevant tests and the project's
lint and type checks against the integrated result. Have the reviewer check
compatibility with existing callers. Keep delivery disabled and return the
result branch, check results, and any compatibility concerns.
```

## Investigate before implementing

```text
Use Horde to investigate why [operation] is slow in [absolute repository path].
Configure a research-only workflow with no implementation or delivery steps.
Reproduce the slowdown with [workload or command], inspect the relevant code,
and record measurements and evidence in a task artifact.
Return the likely cause, the evidence supporting it, and a proposed fix with
a validation plan. Stop after the report; implementation is a separate task.
```

This requires a custom workflow because the default template includes
implementation. Tool permissions and file scopes should match the requested work;
arbitrary commands and external harnesses still rely on cooperative execution.
See [verification and limitations](verification.md#limits-of-this-release).

## Repair a failed check with a bounded workflow

```text
Use Horde to fix the failures from [test or build command] in this repository.
Create a workflow that runs the check, enters one repair step if it fails,
and reruns the same check against the combined changes. Include review of any fix.
If verification still fails, stop and return the failure evidence and result
branch. Do not add further repair cycles. Keep delivery disabled.
```

Use an explicit failure branch for code repair. Retrying a check with `attempts`
only repeats that step. See the [repair template example](templates.md).

## Get status or reconnect to a task

```text
Inspect Horde task [TASK_ID]. Summarize completed and active steps, blocked work,
pending questions, and integration status using the task records and recent events.
Read the task summary for its integrated head and delivery outcome.
Include reported usage and cost; label missing cost data as unknown.
If the task is complete, show the result branch and acceptance check results.
Inspect only; leave the task's execution state unchanged.
```

Use `horde summary TASK_ID` for a snapshot or `horde watch TASK_ID` to follow
events until the task ends. See [progress and notifications](progress.md) for
streaming, summaries, and optional notifications.

## Run implementation on another machine

Replace the machine and model names with choices available to your agent.

```text
Keep planning and review on this machine with [local model pool]. Run the
implementation and its tests on [worker machine] using [worker model pool].
Deliver [feature] with [acceptance criteria]. Choose a suitable model for each
part from those pools; do not run every model just because it is listed.
Keep delivery disabled. Return the result checkout, check results, and any
remaining issues for review.
```

The agent reads reported capabilities before choosing where work runs. Remote
workers need their own provider credentials. Use `horde result TASK_ID` to retrieve
a completed remote result for local review. See [agent-led delegation](delegation.md#let-your-agent-arrange-the-work).

## Add a requirement to existing work

Use this example for a task submitted with delivery disabled. Existing tasks keep
their pinned delivery settings even if you change the repository configuration.

```text
Update Horde task [TASK_ID] with this mandatory requirement from me:
[new requirement]. Record it in the root task's durable context with provenance.
Inspect which results were produced under the earlier context. Append any needed
implementation and verification steps, then resume when the task can proceed.
Check the integrated result against the updated requirement and return the
result branch, validation results, and unresolved issues. Keep delivery disabled.
```

Authoritative context updates invalidate acceptance based on older context.
Completed tasks become blocked; add fresh verification steps and resume to check
the updated result.
The agent should use `update_context` and `add_steps` to record the change.
See [delegation](delegation.md#preserve-intent-without-copying-endless-transcripts).

## Recover after an interruption

```text
Recover Horde task [TASK_ID] after the daemon interruption. Inspect uncertain
attempts, retained worktrees, and any external effects before resuming.
Identify recorded worker processes that are still alive and stop only confirmed
orphaned processes belonging to this task. Reconcile the affected workers once
their processes have exited, then resume work when the evidence supports it.
Report anything you cannot reconcile and any action you need from me.
```

Horde holds uncertain attempts after a hard crash. A plain resume does not settle
whether an interrupted command or external write already happened. See
[recovery and operations](coordination.md#recovery-and-operations).

## Open a pull request and stop before merging

This prompt authorizes pushing the result branch and opening a PR. Use it when
that is the intended delivery boundary and GitHub authentication is configured.

```text
Use Horde to implement [change] in [absolute repository path], with tests and
review. I authorize pushing the result branch and opening a pull request against
[base branch] in [owner/repository]. Configure delivery with merge disabled
before submitting the task. Verify that the repository matches origin.
Use validation appropriate to this project, watch the PR checks, and return
the PR link, check results, and any failures. Stop before merging or deploying.
```

Delivery settings are pinned at submission. The built-in `github-actions`
template includes Next.js/npm checks; other projects need a suitable local
workflow followed by a delivery step. See [GitHub delivery](delivery.md).
