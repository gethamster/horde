---
name: horde-worker
description: Operate correctly as a worker inside a Horde task, or attach an external agent or script to one as a worker. Covers the coordination protocol — registering a worker identity and workspace, claiming exclusive paths before editing, sending and acknowledging mail between workers, raising durable questions instead of guessing, recording artifacts and knowledge with provenance, delegating bounded child work, and committing so the runtime can integrate. Use when a horde coordination MCP server is present, when HORDE_WORKER_TOKEN is set, when instructions mention claims, mailboxes, worktrees, or acceptance criteria from Horde, or when wiring a script or independent harness in as a Horde worker.
---

# Working inside a Horde task

Horde assigns you a task, a worker identity, a scoped token, and your own Git
worktree. Several workers may be editing the same repository at the same time in
different worktrees. The coordination rules below are what keep that from
corrupting the result. They are enforced, not advisory: writes to unclaimed paths
are rejected, and out-of-scope changes are held instead of integrated.

If you are the personal agent submitting and supervising work rather than doing
it, use the `horde` skill instead.

## Are you a worker?

You are, if any of these is true:

- A `coordination` MCP server is configured for you with Horde tools.
- `HORDE_WORKER_TOKEN` is set in your environment.
- Your instructions arrived with an objective, acceptance criteria, a write scope,
  and a worktree path.

Your token limits you to worker-scoped operations. It also forces `task` and
`worker` to your own identity, so you cannot act as another worker even by
accident. See `references/operations.md`.

## The order of operations

Do these in order. Skipping step 2 or 3 makes step 4 fail.

**1. Read your context first.**

```
read_context      { "after": 0, "limit": 25 }
pending_questions { }
read_messages     { }
```

`read_context` gives you the original objective, mandatory constraints, and their
provenance. Your task instructions are narrower than the original request; they do
not replace it. A constraint like "never export email addresses" applies to you
even when your assignment is only "write the CSV writer". Page with the returned
`next` cursor.

**2. Register your workspace.** The runtime allocates your worktree; you declare it
before editing.

```
register_workspace { "path": "/abs/path/to/worktree", "branch": "...", "base": "..." }
```

**3. Claim the paths you will edit.**

```
claim_paths { "paths": ["src/api", "tests/api_test.rs"] }
```

Paths are repository-relative files or directory prefixes. `.` claims the whole
repository. Overlapping another worker's claim is rejected and the rejection tells
you who owns it. Claim before editing, not after. Claim the narrowest scope that
covers your work: a wide claim blocks work that could have run in parallel.

If you need a path someone else owns, message the owner and ask for
`transfer_claim`. Transfers are atomic and also move nested claims. Conversation
alone never moves ownership; only the transfer does.

**4. Work, test, and commit in your worktree.** Then the runtime integrates your
commits into the shared result. Uncommitted work does not exist as far as
integration is concerned.

**5. Report status honestly.**

```
set_worker_status { "status": "working" }   # idle | working | blocked | stopped
```

## Ask instead of guessing

When required information is missing, do not invent it and do not silently pick a
default that changes the contract.

```
request_question {
  "question": "Should the export include soft-deleted rows?",
  "evidence": "src/export.rs:44 filters deleted_at, the spec does not mention it",
  "recommendation": "Exclude them",
  "human_only": false
}
```

This blocks your task only. Other branches keep running. The envelope is durable
and travels up the caller chain unchanged. Set `human_only: true` when a person
genuinely has to decide. Answers reach you at your next tool boundary or
invocation; check `pending_questions` and `read_context` again after being blocked.

## Talk to other workers

```
send_message {
  "id": "unique-client-message-id",
  "destination": "OTHER_WORKER_ID",
  "body": "The response now includes a cursor field",
  "refs": { "file": "src/api.rs" },
  "actionable": true
}
read_messages        { }
acknowledge_messages { "ids": ["unique-client-message-id"] }
```

- `destination` is a worker id, `group:NAME`, or `task`. Join a group with
  `join_channel` first.
- `id` is yours to choose and makes retries safe. Resending the same id with the
  same payload is a no-op; changing the payload under the same id is rejected.
- `actionable: true` wakes an idle managed worker and creates a follow-up task for
  it, keeping the same worker identity. Use it for something the recipient must
  act on, not for status noise.
- Acknowledgement is explicit and per recipient. The cursor never skips unread
  mail, so acknowledge what you have actually processed.
- Read your mail before editing and before declaring done. An interface change
  from another worker is the usual cause of a broken integration.

Message delivery into an already-running CLI harness session is not push-based in
this release. Native workers see mail between tool rounds; harness workers see it
via `read_messages` and in their next invocation context.

## Record what you learned

```
add_knowledge { "kind": "decision", "content": "Chose streaming CSV to bound memory",
                "provenance": { "source": "src/export.rs:44" } }
put_artifact  { "name": "plan", "content": "...", "inputs": { "spec": "sha256:..." } }
reuse_artifact { "name": "plan", "inputs": { "spec": "sha256:..." } }
```

You may report evidence. You may not certify it: setting `verified: true` is
rejected for workers, because verification is the runtime's decision. Check
`reuse_artifact` before regenerating expensive output with an identical input
fingerprint.

## Delegating

If your assignment is genuinely several independent pieces, you can create bounded
children:

```
delegate_task { "id": "export-v1", "objective": "Implement the export component",
                   "template": "local-implementation" }
list_children    { }
integrate_child  { "child": "CHILD_ID", "validation": ["npm", "test"] }
```

`id` is a stable request id: the same assignment retried returns the same child, a
changed assignment under the same id fails. The tree is capped (by default four
active workers, sixteen total children, three levels, two app environments) and
children inherit your constraints and limits.

A child reporting success proves nothing. `integrate_child` imports its commits
relative to the recorded base and runs your `validation` command against the
combined workspace. Conflicts and failed validation are not acceptance. You cannot
finish until your children have current verified acceptance.

A planner-role worker can also reshape its own workflow with `propose_steps`; the
runtime validates the graph and inserts it before the planning task's pending
successors.

## Rules

- Claim before you write. Never edit a path you do not own.
- Never widen your claim to work around a rejection. Ask the owner.
- Commit your work. Integration reads commits, not your working tree.
- Never invent an id: worker, task, question, message, artifact, or child.
- Never mark your task done on the basis of what another worker or child said.
- Never certify your own output as verified.
- Report a failure as a failure, with the evidence. A blocked task with an honest
  question is a better result than a green status over a broken change.

## Attaching an external agent or script

To run your own harness as a Horde worker:

```sh
horde call register_worker '{"task":"TASK_ID"}'
# -> {"id":"WORKER_ID","token":"..."}
```

Give that token to the harness as `HORDE_WORKER_TOKEN` in the environment of its
coordination MCP bridge (`horde mcp`), then register its separate worktree with
`register_workspace` and acquire claims before editing.

Never give a worker the personal-agent bridge. That bridge is administrative and
has no worker scoping.

See `references/operations.md` for every operation a worker token may call, with
arguments.
