# Tasks: lifecycle, results, and recovery

## Submitting

```sh
horde submit "Add CSV export with tests" --repo /path/to/repository
horde submit "Migrate the auth module" --repo . --template nextjs
```

`--repo` defaults to `.` and is canonicalized. `--template` defaults to
`default_template` (`local-implementation`). The response is `{"id": "..."}`.

The equivalent operation, which also accepts initial context records:

```sh
horde call submit_task '{
  "objective":"Add CSV export with tests",
  "repo":"/absolute/path",
  "template":"local-implementation",
  "context":[{"kind":"constraint","content":"Never export email addresses","provenance":"user message 7"}]
}'
```

Preconditions: the repository has an initial commit and a Git author identity, and
every `role` named by the compiled template has an `[executors.<role>]` entry.
Submission fails loudly on either.

Validate a template first if you are unsure:

```sh
horde validate github-actions --repo /path/to/repo
```

## What happens

The template compiles into a dependency graph of tasks. A task becomes eligible
when its dependencies reach terminal states and its `when` condition holds. Steps
with independent dependencies run concurrently when their write scopes permit it.
Unhandled failures skip downstream work. `attempts` bounds retries and each attempt
keeps separate evidence.

Each agent step gets a worker identity, a scoped token, and its own Git worktree.
The worker registers its workspace, claims the paths it will edit, works, commits,
and the runtime integrates its commits into the shared result. Integration is
serialized per task with a file lock and a durable queue record.

Built-in templates: `local-implementation` (plan, implement, review),
`nextjs` (adds `npm ci`, tests, production build on the integrated worktree),
`github-actions` (Next.js verification plus GitHub delivery), `simulated`.

## Monitoring

```sh
horde list                       # all tasks
horde inspect TASK_ID            # tasks, attempts, workers, questions, integration
horde events TASK_ID --after 0   # ordered activity events; pass the last seq to tail
horde metrics TASK_ID            # reported tokens, cost, latency, retries, coordination counts
horde usage                      # account capacity across configured executors
```

`metrics` includes a `children` tree for delegated work. Each node reports its own
execution so aggregates do not double-count. Provider costs that were never
reported stay unknown; unknown is not zero.

For a bot or long-lived agent, use durable event receipts instead of polling from
zero:

```sh
horde call events '{"task":"TASK_ID","after":0}'
horde call ack_events '{"task":"TASK_ID","consumer":"my-agent","seq":42}'
```

Receipts are per-consumer, durable, and monotonic. Acknowledging an event never
answers a question.

## Questions

A worker that lacks required information raises a durable question and blocks only
its own task. Independent branches keep running. `horde inspect` shows it, or:

```sh
horde call pending_questions '{"task":"TASK_ID"}'
horde answer TASK_ID QUESTION_ID "Use CSV and omit identifying fields"
```

The question envelope is immutable. Repeating the same answer is harmless;
a conflicting answer is rejected. A `human_only` question requires the external
caller to assert `human: true`, which is an attestation by your trusted local
integration, not proof of a human. Answering advances the family context version,
which invalidates results pinned to an older version. See `delegation.md`.

## Results

The integrated result is on branch `horde/TASK_ID`. Your original checkout is
untouched and stays on its branch.

```sh
git worktree list
git log --oneline main..horde/TASK_ID
git diff main...horde/TASK_ID
```

Worker worktrees are retained after completion so you can inspect what each one
actually did. Nothing is pushed and no PR is opened unless delivery is enabled
(see `delivery.md`).

Read the diff before you act on it. A completed status means the configured
acceptance steps passed, not that the change is correct for your intent.

## Changing a running workflow

```sh
horde call add_steps '{"task":"TASK_ID","steps":[ ... ]}'
```

`add_steps` validates and appends a new workflow revision without rewriting earlier
attempts. Use it to add a verification step after inspecting evidence, rather than
replaying completed implementation or delivery work.

A planner step running inside the task can call `propose_steps` instead; the
runtime validates the proposed graph and inserts it before the planning step's
pending successors.

## Cancel, resume, and integration failures

```sh
horde cancel TASK_ID    # kills active process groups, cancels the owned subtree
horde resume TASK_ID
```

Merge conflicts abort without changing the existing integrated result. The
conflicting files and commits go back to the owning worker for repair. A clean
merge that fails combined validation is held for repair and does not count as
acceptance. Worktrees are retained in every one of these cases.

## Recovery after a hard crash

A hard daemon crash marks running attempts uncertain and blocks their tasks.
Horde does not assume the interrupted model call, shell command, merge, or external
write did nothing, and it will not replay them automatically.

```sh
horde inspect TASK_ID
# find the uncertain attempt and its worker id, inspect that worktree
# stop any orphaned processes: reconcile refuses while a recorded PID is alive
horde call reconcile_worker '{"task":"TASK_ID","worker":"WORKER_ID"}'
horde resume TASK_ID
```

Claims survive the crash and stay with their owner. After reconciliation they can
be handed off with `transfer_claim` or released with `release_claims`.

On restart Horde also stops owned app and test process groups, removes app `.env`
files it materialized, and tears down owned Compose projects. Where process
identity does not match what it recorded, cleanup is held for inspection rather
than killing an unrelated process that reused a PID.

## Artifacts and knowledge

Artifacts are content-addressed by SHA-256, synced before their database reference
commits, and verified on retrieval. Each link records its input fingerprint and
verification status, so identical inputs can find a verified prior result:

```sh
horde call put_artifact '{"task":"TASK_ID","name":"plan","content":"...","inputs":{"spec":"sha256:..."},"verified":true}'
horde call reuse_artifact '{"task":"TASK_ID","name":"plan","inputs":{"spec":"sha256:..."}}'
horde call get_artifact '{"task":"TASK_ID","hash":"..."}'
```

`inputs` is an object fingerprint of what produced the content. `reuse_artifact`
returns a verified artifact only when that fingerprint matches exactly.

Knowledge records facts, decisions, and evidence with provenance and relationships:

```sh
horde call add_knowledge '{"task":"TASK_ID","kind":"decision","content":"CSV over XLSX","provenance":{"source":"caller message 7"},"verified":true}'
horde call knowledge '{"task":"TASK_ID"}'
horde call link_knowledge '{"task":"TASK_ID","source":"KID_A","target":"KID_B","relation":"supports"}'
```

Execution state is never inferred from a knowledge claim or from conversation. A
worker saying it finished proves nothing; the attempt record and the integration
result do.
