# Delegation, context, and questions

Horde owns execution. Your agent or bot owns the user conversation and connects
through the same CLI or MCP interface. There is no chat integration, webhook, or
message store inside Horde.

## One bounded tree

A root task and everything it spawns is a single bounded tree:

```toml
[limits]
workers = 4          # active workers across the whole tree
children = 16        # every descendant ever created, finished ones included
depth = 3            # levels below the root
environments = 2     # concurrent app environments
```

Limits are pinned at submission and inherited. Children cannot reset them. Each
remote child reserves one root worker slot, and uncertain work keeps its
reservation until reconciliation. This bounds expansion; it is not a distributed
scheduler.

```sh
horde call delegate_task '{
  "task":"PARENT_ID",
  "id":"export-v1",
  "objective":"Implement the export component",
  "template":"local-implementation"
}'
horde call list_children '{"task":"PARENT_ID"}'
```

`id` is a stable request id used for deduplication. Retrying the exact same
assignment returns the existing child. Reusing the id with a changed assignment
fails. Omit `peer` for local work; supply an enrolled peer id for remote work, and
commit your current work first because a child starts from the caller's committed
snapshot in its own repository and worktrees. `bundles` narrows inherited app
secrets to a subset or to `[]`; a child can never widen them.

## Context travels, transcripts do not

Every invocation receives the original objective, mandatory constraints, source
ids, provenance, answered question envelopes, and a context version. The narrower
task instructions accompany that contract, they do not replace it. A child working
on one component still knows what the caller actually asked for.

```sh
horde call read_context '{"task":"CHILD_ID","after":0,"limit":25}'
horde call update_context '{
  "task":"ROOT_ID",
  "kind":"constraint",
  "content":"Never export email addresses",
  "provenance":"Original caller message 7"
}'
```

Use the returned `next` cursor to page. Mandatory sources are capped at 256 KiB in
total and a single update at 64 KiB. When context grows too large, the root caller
appends a consolidated source with `supersedes` listing the old source ids: old
sources stay retrievable and the original objective can never be superseded.

Authoritative updates and answers advance the family context version. An attempt
that started under an older version cannot have its result accepted. A completed
task becomes blocked instead: add a fresh verification step with `add_steps` and
resume, rather than replaying completed implementation or delivery effects.

Supporting facts belong in `add_knowledge` with their own provenance, verification
status, and input references. A source reference lets you check the original
evidence; it does not prove a model understood it.

## Questions travel one caller at a time

A worker calls `request_question` with `question` and optional `id`, `evidence`,
`recommendation`, and `human_only`. The envelope is durable and immutable. Its
immediate caller reads `pending_questions` and picks exactly one action:

```sh
horde call answer_question '{"task":"CALLER_ID","question":"QUESTION_ID","answer":"Use CSV and omit identifying fields"}'
horde call escalate_question '{"task":"CALLER_ID","question":"QUESTION_ID","commentary":"This changes the requested export contract"}'
```

Escalation moves the same question up one level. Commentary stays separate; no
caller rewrites a question into a different question. The worker that created a
child decides that child's questions, and another worker cannot take that
authority. Root questions surface to the external CLI or MCP caller, which may
answer within its own authority or fetch the user's answer from whatever chat
surface it owns.

`human_only` questions require `"human": true` from the external caller. That is
an attestation by your trusted local integration, not authentication of a human.

An unanswered question holds only its assigned worker's task; unrelated branches
keep running. Native workers see pending questions at tool boundaries; CLI
harnesses see them through MCP and their next invocation context. Repeating an
identical answer is harmless, a conflicting one is rejected.

For a reconnecting bot: call `events` with a stable `consumer` string, process what
comes back, then `ack_events` with the last `seq`. Receipts are durable and
monotonic, and acknowledging an event does not answer a question. Persist your own
mapping from root task ids to chat threads.

## Acceptance stays with the parent

```sh
horde call integrate_child '{"task":"PARENT_ID","child":"CHILD_ID","validation":["npm","test"]}'
```

A child success report is provisional. The parent imports the child's changes
relative to the recorded base, serializes integration, and runs the supplied
command against the combined workspace.

- Conflicts return evidence without overwriting the parent's work.
- A clean merge that fails combined validation is held for repair and records no
  acceptance.
- A parent cannot finish until every child has current, verified acceptance.
- Cancelling a parent cancels its owned subtree.
- Remote acceptance is deduplicated by owner identity and assignment hash. A lost
  response retries the pinned manifest; a changed assignment under the same id
  fails.

Never mark delegated work done on the strength of a status field. Integrate it and
run something that would fail if it were wrong.
