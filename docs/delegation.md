# Delegation and the original caller

Horde owns execution. Your agent or bot owns the user conversation and connects
through the same CLI or stdio MCP interface. There is no Slack-specific server,
public webhook, or extra chat store to configure inside Horde.

## Let your agent arrange the work

Describe the work split in your coding session. For example, ask for thinking on
the local machine using Codex and Astra, with delivery on Apollo using a pool of
Claude, Codex, and GLM 5.3. The parent chooses which allowed model suits each task.
Horde does not interpret that list as a requirement to invoke every model.

The shipped skills guide the agent through `runtime_capabilities` and
`plan_execution`. Discovery includes each worker's reported executors and models,
available capacity, freshness, and authentication evidence. A credential's
presence does not prove the provider will accept it. Ambiguous names, missing
models, and stale workers require resolution before a scoped assignment proceeds.

`agent_setup` exposes local setup actions and precise missing requirements. Your
agent uses its available execution or platform tools to deliver enrollment
credentials and start remote workers. Horde cannot create access to a machine or
provider that the agent does not have. Fleet startup accepts the same secret
reference across containers, Kubernetes, sandboxes, VMs, and individual machines.

After briefly explaining its work split, the parent submits or delegates with an
`execution` policy. Each allowed entry binds a runtime to a set of capability IDs;
`selected` chooses one pair for that task. Child policies may narrow their parent's
pool. The receiving worker checks the pinned model/provider binding against its
own configuration and supplies its own credentials. Drift fails explicitly.
Horde preserves the assignment through retries and never moves it to local work
because a worker disconnects.

The agent supplies a stable `request_id` to `submit_task`, or `id` to
`delegate_task`. If a reply is lost, retrying that same request returns its existing
task before looking up live workers again. Reusing an ID for different work fails.
Task inspection includes the pinned execution policy for review.

The parent follows progress and questions through the existing task tree, checks
the returned changes, and reports a reviewable result. Merge and deployment need
their own authorization. Lasting changes to this behavior go through discussed
project skill proposals; see [runtime skills](runtime-skills.md).

## Run a task on a worker

Choose a worker by its name from `horde runtime list`:

```sh
horde submit --on apollo --repo /path/to/repo "Run the tests and fix failures"
```

The returned task ID belongs to the controller. Use the usual `horde inspect`,
`horde events`, `horde metrics`, and `horde cancel` commands with that ID. The
controller sends a committed repository snapshot through the worker's existing
connection. It does not start local executor steps or fall back to local execution
if that worker disconnects. Unknown or ambiguous names fail before a task is
created. An offline enrolled worker keeps its queued work until it reconnects.

Once remote execution succeeds, retrieve a local checkout for review:

```sh
horde result TASK_ID
```

The result includes the checkout path. Your original repository and branch remain
unchanged. Repeating the command returns the same checkout, but refuses to
replace it if you edited it. Remote execution success does not certify those
changes for a local merge. Review and test the result before applying it.

A remote task cannot be resumed as local work. Failed or interrupted remote work
retains its existing recovery state; submit a new task explicitly when a new run
is appropriate. Workers need their own provider configuration and credentials.

## One bounded tree

Configure defaults in user settings or `.horde/horde.toml`:

```toml
[limits]
workers = 4
children = 16
depth = 3
environments = 2
```

`children` counts all descendants ever created, including finished work. `depth`
counts levels below the root. Limits are pinned at submission and inherited;
children cannot reset them. Each remote child reserves one worker slot. Uncertain
work keeps its reservation until reconciliation. Remote app steps obtain a root
environment lease before starting. Per-daemon concurrency can restrict work
further. This bounds expansion; it is not a distributed scheduling service.

```sh
horde call delegate_task '{"task":"PARENT","id":"export-v1","objective":"Implement the export component","template":"local-implementation"}'
horde call list_children '{"task":"PARENT"}'
```

Supply a stable request `id`. Retrying the exact assignment returns the existing
child; changing the assignment with that ID fails. Omit `peer` for local work or
supply an enrolled peer for remote work. A child starts from the caller's
committed snapshot and uses a separate repository and worktrees. Remote callers
must commit their current work before delegation.

## Preserve intent without copying endless transcripts

Each invocation gets the original objective, mandatory constraints, source IDs,
provenance, answered question envelopes, and a context version. The narrower step
instructions accompany that contract. Supporting facts and evidence have their
own source catalog and are fetched in bounded pages:

```sh
horde call read_context '{"task":"CHILD","after":0,"limit":25}'
horde call update_context '{"task":"ROOT","kind":"constraint","content":"Exclude email addresses","provenance":"Original caller message 7"}'
```

Use the returned `next` cursor to retrieve another page. Mandatory sources have a
256 KiB total limit; a single update is at most 64 KiB. If context becomes too
large, the root caller can append a consolidated source with `supersedes` listing
old source IDs. Old sources remain retrievable, and the original objective cannot
be superseded. Supporting knowledge is recorded with `add_knowledge`, including
provenance, verification status, and input references. Use `scope = "family"` to
publish it across the tree, and query it with `knowledge`; it is not injected into
inherited context. See [task-family notebooks](coordination.md#task-family-notebooks).

Authoritative updates and answers advance the family version. An attempt that
started with an older version cannot have its result accepted. Completed tasks
become blocked; add a fresh verification step with `add_steps` and resume rather
than replaying completed implementation or delivery effects. Source references
support checking the original evidence; they do not prove a model understood it.

## Questions travel one caller at a time

A worker calls `request_question` with `question`, optional `id`, `evidence`,
`recommendation`, and `human_only`. The original envelope is durable. Its immediate
caller reads `pending_questions`, then chooses one action:

```sh
horde call answer_question '{"task":"CALLER","question":"QUESTION_ID","answer":"Use CSV and omit identifying fields"}'
horde call escalate_question '{"task":"CALLER","question":"QUESTION_ID","commentary":"This changes the requested export contract"}'
```

Escalation moves the same question one level upward. Commentary stays separate;
no caller rewrites the question into a new question. The worker that created a
child can decide its questions; another worker cannot take over that authority.
Root questions surface to the external CLI/MCP caller. That caller may answer
within its own authority or obtain the user's answer in Grokbot, Slack, or another
UI. A `human_only` question requires `human: true` from the external caller. This
is an attestation by the trusted local integration, not authentication of a human.

An unanswered question holds its assigned worker's step, allowing unrelated steps
to continue. An idle caller worker is notified. Native workers receive questions
at tool boundaries; CLI harnesses check MCP and their next invocation context.
Repeated identical answers are harmless; conflicting answers are rejected.

For a reconnecting bot, use `events` with a stable `consumer` string, deliver or
process the returned events, then call `ack_events` with the last `seq`. Receipts
are durable and monotonic. Event acknowledgement does not answer a question.
The bot should persist its own mapping from root task IDs to chat threads.

## Acceptance stays with the parent

```sh
horde call integrate_child '{"task":"PARENT","child":"CHILD","validation":["npm","test"]}'
```

A child success report is provisional. The parent imports changes relative to the
recorded base, serializes integration, and runs the supplied command against the
combined workspace. Conflicts return evidence without overwriting the parent's
work. Clean merges that fail combined tests remain held for repair and do not
record child acceptance. A parent cannot finish until its children have current
verified acceptance. Cancelling a parent cancels its owned subtree.

`metrics` includes a `children` tree. Completed remote children carry the executor's
reported metrics; unknown provider costs remain unknown. Each node reports its own
execution, so callers can aggregate the tree without counting parent totals twice.
