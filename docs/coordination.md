# Agent connections and coordination

## Connect your agent

Configure a stdio MCP server with command `horde` and arguments `mcp`. For a custom data directory, arguments are `--data-dir`, `/absolute/path`, `mcp`.

```json
{
  "mcpServers": {
    "horde": { "command": "horde", "args": ["mcp"] }
  }
}
```

The personal-agent bridge exposes submit, inspect, events, questions, cancellation, resumption, metrics, revisions, artifacts, knowledge, and coordination tools. Internal harness bridges receive a worker token and expose only worker-scoped operations. Native tools and external MCP tools use the same coordination handlers.
Worker schemas omit runtime identity, step attribution, and verification fields.
The runtime accepts matching identity fields, rejects mismatches, and discards a
supplied `verified` flag with a warning; workers cannot certify their own evidence.

For a script or independent harness, create a worker with `register_worker`, then register its separate worktree using `register_workspace` (`path`, `branch`, `base`). Set the returned token as `HORDE_WORKER_TOKEN` in its MCP bridge environment. Acquire claims before editing. Never share the personal-agent bridge with an untrusted worker.

### Command-step identity

Every ordinary `kind = "command"` step receives its task, step, attempt, and worker
UUIDs as `HORDE_TASK_ID`, `HORDE_STEP_ID`, `HORDE_ATTEMPT_ID`, and `HORDE_WORKER_ID`.
`HORDE_WORKER_TOKEN` authenticates calls with the same scope as an agent worker.
`HORDE_BIN` names the executing Horde binary, and `HORDE_DATA_DIR` selects its daemon
for CLI calls; an explicit `--data-dir` still takes precedence.

For example, a command can publish its measurement directly:

```sh
"$HORDE_BIN" call add_knowledge '{"id":"cell-verdict","scope":"family","topic":"testing","kind":"evidence","content":"The fixture passed","provenance":{"source":"cell"},"valid_under":{"dataset":"fixture-v2"}}'
"$HORDE_BIN" call put_artifact '{"name":"verdict","content":"The fixture passed"}'
```

Choose IDs appropriate for the claim and retry; a changed claim cannot reuse an ID.
If the task declares `knowledge_topics`, the topic must be in that vocabulary.
Calls supply task/step attribution automatically. Workers cannot self-verify,
impersonate another task or step, revise another task's claims, or use administrative
operations. Question answering keeps the existing caller and human-only rules.
The token rotates on retry and is redacted from captured stdout/stderr. It is not
a provider API key. Other application environment values retain their existing
bundle checks and redaction. Nested native commands and environment lifecycle
commands do not gain these credentials merely by using the command runner.

## Messages and ownership

Every operation is available as `horde call OPERATION 'JSON'`. Examples:

```sh
horde call register_worker '{"task":"TASK_ID"}'
horde call list_workers '{"task":"TASK_ID"}'
horde call claim_paths '{"task":"TASK_ID","worker":"WORKER_ID","paths":["src/api"]}'
horde call send_message '{"task":"TASK_ID","worker":"WORKER_ID","id":"unique-client-message-id","destination":"OTHER_WORKER_ID","body":"The response now includes a cursor","refs":{"file":"src/api.rs"},"actionable":true}'
horde call read_messages '{"task":"TASK_ID","worker":"WORKER_ID"}'
horde call acknowledge_messages '{"task":"TASK_ID","worker":"WORKER_ID","ids":["unique-client-message-id"]}'
```

Destinations are a worker ID, `group:NAME`, or `task`. Join a group with `join_channel`. Broadcast recipients are snapshotted at send time. A `task` broadcast reaches every other worker; when the sender is the only worker on the task, it is delivered to the sender itself so a solo planner still hears the message. Retrying the same message ID with the same payload is idempotent; changing its payload is rejected. Acknowledgement is explicit and per recipient, with a cursor that never skips unread mail.

Operators steer a running task without a worker identity. List workers first, then fan out or target one:

```sh
horde call list_workers '{"task":"TASK_ID"}'
horde steer TASK_ID "Prefer the streaming parser; skip the CLI flag"
horde steer TASK_ID "Only you: re-check the parser" --worker WORKER_ID
horde steer TASK_ID "Status update only" --presence
```

Steering posts as `operator:TASK_ID`. Omit `--worker` (alias `--to`) to reach every worker on the task, including a single worker; pass `--worker ID` to deliver only to that worker. It is actionable by default; `--presence` delivers without waking idle workers. Messages from `operator:TASK_ID` are operator instructions, not peer chat; workers cannot reply to that identity. Steering requires operator credentials and fails when the task has no workers, the target worker is unknown or on another task, the target is the operator identity, or the task is cancelled.

Actionable messages notify idle managed workers and create a follow-up step, retaining worker identity. Presence messages and acknowledgements do not invoke models. Native workers receive unread messages at each model/tool round; harnesses receive a launch prompt and coordination MCP tools. Continuous push into an already-running CLI harness is not available in this release.

Claims use repository-relative files or directory prefixes. `.` claims the whole repository. Overlap is rejected with ownership evidence. `transfer_claim` is atomic. Claims survive crashes; conversation alone never transfers ownership. Native file and patch tools check claims. External edits and native commands are checked before integration, with out-of-scope results held for reconciliation.

## Recovery and operations

For upgrades from an older database layout, see [database repair](installing.md#recovering-an-older-database).
The repair path preserves a backup and holds unfinished work for inspection.

```sh
horde cancel TASK_ID
horde inspect TASK_ID
horde call reconcile_worker '{"task":"TASK_ID","worker":"WORKER_ID"}'
horde resume TASK_ID
```

A hard daemon crash marks active attempts uncertain and blocks their tasks. `reconcile_worker` refuses while a recorded worker process is alive. Inspect the worktree and external effects, stop orphan processes, then reconcile and resume. This intentionally avoids replaying uncertain shell/model operations automatically. Claims remain available for handoff or explicit release after the process is reconciled. A graceful stop or cancellation kills each active process group.

Git integration is serialized per task. Merge conflicts are aborted without changing the existing integrated result; conflicting files and commits are sent back to the owner. A failed combined validation holds the combined changes for repair. Worktrees are retained for inspection.

## Local service and data

`horde stop` shuts the service down gracefully. `horde daemon` runs in the foreground; `horde start` detaches it and writes `daemon.log`. The default data directory is `~/.local/share/horde`. Set `--data-dir PATH` consistently on every command to use a different instance. Keep this path short enough for a Unix socket (under roughly 90 characters on macOS).

`add_knowledge.kind` accepts exactly `fact`, `decision`, or `evidence`. These values
are enumerated in both the worker and administrative tool schemas; unsupported
values return an error listing the valid choices.

## Task-family notebooks

Knowledge records are claims with provenance. They never change the family context
version, advance execution, satisfy acceptance, or unblock a step. Horde does not
inject notebook contents into prompts. A task's instructions or harness decides
when to query them.

`add_knowledge` accepts `scope = "task"` (default) or `scope = "family"`. A task
owns every record it writes. Family scope publishes it to descendants, ancestors,
and siblings under the same root, including remote tasks. Another root using the
same repository cannot read it. Task scope remains private to the originating task.

```json
{
  "scope": "family",
  "kind": "evidence",
  "topic": "testing",
  "content": "The migration fails when the source table is empty.",
  "provenance": {"test": "empty_source", "artifact": "test-report"},
  "valid_under": {"commit": "abc123", "dataset": "fixture-v2"}
}
```

Worker credentials supply task and step attribution. `origin` records that identity;
remote writes retain the authoritative task ID, sending runtime, remote task ID,
and source step. Writer-supplied `provenance` and `valid_under` remain separate from
runtime attribution. Horde stores conditions without interpreting them. Workers
cannot set `verified`; a claim's verification flag is not an execution result.
Provide an optional `id` when retrying a write after a lost reply. Repeating that ID
with the same claim is idempotent; changing its content or supersession list fails.

### Querying and exporting

Pass `scope` to `knowledge` to request a page:

```json
{"scope":"family","topic":"testing","query":"migration","limit":50}
```

The response has `records`, `next`, and a notebook `revision`. Pass the returned
`next` string as `after`, keeping the same scope, query, and filters; null `next`
means the last page. Pages default to 50 records, allow at most 100, and bound record
payloads to 256 KiB. Full-text queries use SQLite FTS5 syntax and return ranked
matches (lower `rank` values rank first); without a query, records follow insertion
order. Topic filtering uses exact strings.

Cursors bind the task, filters, and notebook revision. A knowledge write or lifecycle
change invalidates existing cursors, including changes elsewhere in that runtime's
FTS index. Restart without `after` when told the notebook changed. Horde returns an
error instead of silently mixing result versions or skipping ranked matches.

`scope = "family"` returns published family rows only. `scope = "task"` returns
rows owned by the caller's task, including its own family publications. For backward
compatibility, a `knowledge` call without any scope, query, or paging options retains
the original task-only array response. Use explicit scope for new consumers.

Each paged row includes visible outgoing `edges`. If `edges_next` is non-null,
continue with `knowledge_edges`, passing the row ID as `source` and that cursor as
`after`. Relationship pages retain provenance IDs and hide private targets.
A final command or agent step can export these pages into the repository's own
memory files. Horde does not promote that export into a global knowledge store.

### Topics and claim lifecycle

A repository can declare `knowledge_topics` in `.horde/horde.toml`. The vocabulary
is pinned with the root's settings. It appears as an enum in native and worker MCP
schemas; an administrative client can call `knowledge_options` for a task's topics,
scopes, kinds, and specialized schemas. An empty vocabulary permits free-form topics.
Topic is optional; a supplied topic must match the configured vocabulary.

`add_knowledge.supersedes` lists older IDs. Supersession must preserve scope and
may target only active claims owned by the caller's task, or visible claims when an
operator makes the call. A sibling can publish contradictory evidence and link its
own claim using `link_knowledge`, but cannot supersede or retract another task's claim.

`retract_knowledge` takes `id`, `reason`, and `provenance`, retaining the original
claim and its withdrawal record. Superseded and retracted rows are excluded by
default; `include_inactive = true` returns them with explicit markers and
`superseded_by` or `retraction` details. A relationship label alone does not change
lifecycle flags; use the explicit supersession or retraction operation. Links never
confer execution authority.

### Storage and remote access

Schema 5 adds notebook metadata and a local FTS5 index. Existing knowledge rows
remain task-local and are indexed on upgrade. Earlier versions also copied knowledge
into supporting context; context reads now hide those legacy copies from other
tasks. New notebook writes do not create context sources or prompt entries.

Remote notebook operations use the existing authenticated caller route to the
owning runtime. They require notebook support at that authority and fail explicitly
if it is unavailable or too old; they do not return a stale local approximation.
No model provider or embedding service is required.
