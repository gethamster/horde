# Delegation and the original caller

Horde owns execution. Your agent or bot owns the user conversation and connects
through the same CLI or stdio MCP interface. There is no Slack-specific server,
public webhook, or extra chat store to configure inside Horde.

## One bounded tree

Configure defaults in user settings or `.horde.toml`:

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
provenance, verification status, and input references.

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
