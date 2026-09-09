---
name: horde
description: Run long, parallel, or risky coding work on Horde, a local daemon that assigns software tasks to coding agents and survives your session ending. Use when a task needs to keep running after you exit, needs several agents editing one repository at once without collisions, needs a plan/implement/review pipeline, needs a different model than the one you are running on, or needs a durable record of what each attempt did. Also use to install Horde, connect it over MCP, configure executors and concurrency, submit and monitor tasks, answer agent questions, collect results from worktrees, and recover after a crash. Triggers include "horde", "horde.sh", "run this in the background", "delegate this task", "run these in parallel", "keep working while I'm gone", "hand this to another agent".
---

# Horde

Horde is a local Rust daemon that owns scheduling for coding work. You submit an
objective and a repository; Horde expands it into a durable workflow, runs coding
agents in isolated Git worktrees, records every attempt, and integrates the result
onto a separate branch. It keeps running after the client that submitted the work
exits.

You keep your own agent. Horde does not replace this session. It is the place you
put work that should outlive it.

## Workflow guidance and project overrides

Horde ships file-based guidance for setup, discovery, model selection, planning,
delegation, and review. Use `skill_inspect` with the repository and
optional skill name to read the effective project guidance. `runtime_capabilities`
and `plan_execution` resolve the user's requested local or remote model pools;
the caller chooses which permitted capability each bounded task needs. Do not
run every listed model merely because it appears in the pool.

Use `request_id` for `submit_task` and a stable `id` for `delegate_task`. Retry the
original identifier and intent after a timeout so uncertain work is not duplicated.
For persistent guidance changes, present the `skill_propose` diff and wait for
explicit acceptance before `skill_apply`. `skill_rollback` also produces a
proposal for review. Existing tasks and children retain their pinned versions.
Use `skill_pack_install` with an absolute directory path to install edited skill
files without changing the binary. `runtime_skills_update` sends that default pack
to a named worker; `runtime_update` requests a signed binary version. Both remote
operations take `id` and a stable `request_id`. Inspect `runtime_inspect.operations`
until the request succeeds and check the reported version or pack hash. Preserve
running task pins and keep requests within the user's authorized update scope.
See [runtime skills](https://github.com/gethamster/horde/blob/main/docs/runtime-skills.md)
for scope and API details.

## Why it exists

An agent session is ephemeral, single-threaded, and forgetful. That is fine for a
small edit and wrong for everything else. Horde fixes five specific failures:

1. **Work dies with the session.** Horde's state is SQLite in WAL mode with FULL
   synchronous writes. The daemon owns the schedule. Disconnecting a CLI or MCP
   client does not cancel anything.
2. **Parallel agents corrupt each other.** Every worker gets its own worktree and
   must acquire an exclusive claim on the paths it edits. Overlapping claims are
   rejected with ownership evidence. Integration into the shared result is
   serialized per task.
3. **A crash silently replays side effects.** Horde never assumes an interrupted
   model call, shell command, merge, or GitHub write did nothing. It marks the
   attempt uncertain, blocks the task, and requires explicit reconciliation.
4. **One model does everything.** Roles (`planner`, `worker`, `reviewer`, and any
   role you name) each map to an executor: Codex CLI, Claude Code CLI, a Tuara
   API model, or a simulated no-op. Mix them per step, with configured fallback
   when an account runs out of capacity.
5. **Agents guess instead of asking.** A worker can raise a durable question that
   holds only its own task. The question travels up the caller chain, unchanged,
   to you.

Everything Horde reports is evidence-backed. A child reporting success is
provisional until the parent imports its commits and runs combined validation.

## When to use it, and when not to

Use Horde when the work is long-running, needs more than one agent, needs a
verification pass by a different model, needs to survive a restart, or needs an
auditable record of attempts.

Do not use Horde for a change you can make in this session in a couple of edits.
Do not use it as a security sandbox: it is a cooperative, single-user runtime, not
an isolation boundary against hostile code running as the same user. Do not expect
a dashboard, a distributed scheduler, a hosted service, or a team-shared instance.

## Setup

This is the whole path from nothing to a working Horde. Run it end to end; do not
hand the user a list of commands to run themselves.

**1. Is it already there?**

```sh
horde --version
```

If that works, skip to step 4.

**2. Install.** Installing a daemon on someone's machine is their decision, so
confirm with the user first. Requires macOS or Linux and Git.

```sh
curl -fsSL https://horde.sh/install | bash -s -- --no-service
```

`--no-service` matters. Without an explicit `--service` or `--no-service` the
installer asks whether to start Horde at login, and that prompt falls back to
reading `/dev/tty` when stdin is not a terminal, which is exactly what
`curl | bash` is. An agent tool call blocks there with no way to answer. Pass the
flag and the question never happens.

Boot startup stays a separate, reversible step the user opts into later:

```sh
horde service install     # horde service uninstall to undo
```

**3. Configure an executor.** Horde needs a coding agent to run the work. Installing
already wrote `~/.config/horde/config.toml` and a private `credentials.env`; run
`horde config init` if either is missing.

Out of the box `planner`, `worker`, and `reviewer` go through `providers.default`,
which is Tuara over an API key. Store the key without editing a file:

```sh
printf '%s\n' "$KEY" | horde config provider add tuara --key-stdin
```

Use `--key-stdin` from an agent: the bare `horde config provider add` walkthrough
prompts on the terminal and will block a tool call with no way to answer.

To use a CLI already on the machine instead, point those roles at the provider that
matches it. Both ship with `auth_mode = "login"`, so they use the CLI's own
credential store and need no key:

```sh
command -v codex claude
horde config provider add claude --use-for planner,worker,reviewer   # or codex
```

Check what landed, and what a provider will accept as a model:

```sh
horde config provider list
horde config models default
```

A role may also set `model` to pick what that provider is asked for. See
`references/configuration.md`.

**4. Start the daemon.**

```sh
horde start
```

Keys in `credentials.env` are re-read for each invocation, so `horde config provider
add` takes effect on the next task with no restart. A key exported into the daemon's
environment instead is fixed for the life of that process, so changing one needs
`horde stop && horde start`. An exported variable also wins over the file: a stale
`TUARA_API_KEY` in the daemon's shell makes edits to `credentials.env` look ignored.

**5. Prove it works, for free.**

```sh
cd /path/to/a/git/repo
horde submit "Exercise the runtime" --repo . --template simulated
horde inspect TASK_ID
```

The `simulated` template exercises scheduling, worktrees, and integration with no
model calls and no cost. If it returns an id and the tasks complete, the runtime
is sound and any later failure is configuration or the model, not Horde.

**6. Wire it into this agent** (below), then run a real task.

Confirm each step succeeded before starting the next, and report what actually
happened. See `references/setup.md` for MCP wiring per agent, data directories,
and troubleshooting.

## The loop

```sh
horde submit "Add CSV export with tests" --repo /path/to/repository   # -> {"id":"..."}
horde watch TASK_ID        # NDJSON stream until the task ends; exit 0 succeeded, 1 failed, 2 cancelled
horde inspect TASK_ID      # tasks, attempts, workers, questions, integration
horde events TASK_ID       # ordered activity, use --after SEQ to tail
horde summary TASK_ID      # status, step outcomes, integrated head, pr_url or delivery_skipped
horde metrics TASK_ID      # tokens, cost, latency, retries, coordination counts
```

`horde watch` blocks, so run it when you can wait; `--timeout-secs N` returns
exit 3 instead of waiting forever, and `--after SEQ` resumes a stream. Its last
line is the same object `horde summary` prints. Read `delivery` there before
reporting: `pr_ready` carries a PR URL, and `delivery_skipped` says why no PR
exists. A `[notify]` table in the configuration pushes the same milestones to a
webhook or command; `docs/progress.md` in the Horde repository documents the
stream, the summary object, and the payload.

Rules that matter:

- The repository must have an initial commit and a configured Git author.
- Coding happens in separate worktrees. Your checkout stays on its branch.
- The integrated result lands on branch `horde/TASK_ID`. Find it with
  `git worktree list`. Diff it before you trust it.
- Nothing is pushed and no PR is opened unless delivery is explicitly enabled.
- Settings are pinned at submission. Editing config later affects new tasks only.

When a worker needs a decision, `horde inspect` shows a pending question:

```sh
horde answer TASK_ID QUESTION_ID "Use CSV and omit identifying fields"
```

Answer within your authority or, if you are an intermediate caller, escalate the
original envelope one level with `escalate_question`. Never rewrite a question
into a different question.

To stop or restart work:

```sh
horde cancel TASK_ID
horde resume TASK_ID
```

## After a crash

A hard daemon crash marks running attempts uncertain and blocks their tasks.
This is deliberate. Do not try to force it forward.

1. `horde inspect TASK_ID` to find the uncertain attempt and its worker.
2. Inspect that worker's worktree and any external effects (pushed branches, open
   PRs, running app processes).
3. Stop orphaned processes. `reconcile_worker` refuses while a recorded process is
   still alive.
4. `horde call reconcile_worker '{"task":"TASK_ID","worker":"WORKER_ID"}'`
5. `horde resume TASK_ID`

Claims survive the crash and stay with their owner until reconciled and released.

## Where the rest lives

Read the reference only when you need it.

| Need | File |
| --- | --- |
| Install, MCP wiring per agent, boot service, data directories, troubleshooting | `references/setup.md` |
| Settings TOML, executor roles, auth modes, credential broker, concurrency, limits | `references/configuration.md` |
| Task lifecycle, results, revisions, artifacts, knowledge, recovery in depth | `references/tasks.md` |
| Bounded delegation trees, inherited context, question routing, child acceptance | `references/delegation.md` |
| App `.env` bundles, disposable process and Compose test environments | `references/environments.md` |
| GitHub push, PR, checks, merge, deployment, health delivery | `references/delivery.md` |
| Tailscale/mTLS networking, managed E2B/Daytona/Docker/Kubernetes runtimes, updates | `references/fleet.md` |
| Every operation, its arguments, and whether a worker token may call it | `references/operations.md` |

Related skills: `horde-templates` to author workflow templates, `horde-worker`
for an agent running as a worker inside a Horde task.

## Non-negotiables

- Never invent a task, worker, question, or artifact id. Read it from output.
- Never report a task as done because a child or worker said so. Confirm through
  `inspect` and, for delegated work, through `integrate_child` with real validation.
- Never bypass reconciliation after a crash or an uncertain attempt.
- Never put provider API keys in an app secret bundle. Provider credentials belong
  in executor configuration; bundles are for the software being built.
- Never share the personal-agent MCP bridge with a worker.
- Set provider API keys in the **daemon's** environment before `horde start`.
  Worker command environments use an allowlist that omits them.
- Report what the evidence shows, including unknown cost and unknown capacity.
  Unknown is not zero.
