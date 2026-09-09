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

Destinations are a worker ID, `group:NAME`, or `task`. Join a group with `join_channel`. Broadcast recipients are snapshotted at send time. Retrying the same message ID with the same payload is idempotent; changing its payload is rejected. Acknowledgement is explicit and per recipient, with a cursor that never skips unread mail.

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
