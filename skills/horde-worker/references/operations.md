# Worker operation reference

Everything here is callable with a worker token, as an MCP tool on the
coordination bridge or as `horde call OPERATION 'JSON'` when the token is in the
environment. `*` marks required fields.

`task` and `worker` are supplied by your token. Do not pass a different value:
the RPC layer rejects it as a scope violation. Arguments beginning with `_` are
rejected as reserved.

## Context and questions

| Operation | Arguments |
| --- | --- |
| `read_context` | `after`, `limit` — paged; use the returned `next` cursor |
| `pending_questions` | none |
| `request_question` | `question`*, `id`, `evidence`, `recommendation`, `human_only` |
| `answer_question` | `question`*, `answer`*, `human` — only for questions from your own children |
| `escalate_question` | `question`*, `commentary` — forwards the original envelope one level up |

## Workspace and ownership

| Operation | Arguments |
| --- | --- |
| `register_workspace` | `path`*, `branch`*, `base`* |
| `claim_paths` | `paths[]`* — repository-relative files or directory prefixes; `.` is the whole repo |
| `transfer_claim` | `to`*, `path`* — atomic, moves nested claims too |
| `list_workers` | none |
| `set_worker_status` | `status`* — `idle`, `working`, `blocked`, `stopped` |

Not available to a worker token: `release_claims`, `reconcile_worker`, `integrate`,
`register_worker`. Those are caller or runtime operations.

## Mail

| Operation | Arguments |
| --- | --- |
| `send_message` | `id`*, `destination`*, `body`*, `refs{}`, `actionable` |
| `read_messages` | `after`, `limit` |
| `acknowledge_messages` | `ids[]`* |
| `join_channel` | `channel`* |

`destination` is a worker id, `group:NAME`, or `task`. Broadcast recipients are
snapshotted at send time. Delivery is at-least-once until you acknowledge
explicitly. A separate notification watermark stops an already-delivered wakeup
from invoking a model twice.

## Artifacts and knowledge

| Operation | Arguments |
| --- | --- |
| `put_artifact` | `name`*, `content`*, `inputs{}`, `step` |
| `get_artifact` | `hash`* |
| `reuse_artifact` | `name`*, `inputs{}`* |
| `add_knowledge` | `kind`*, `content`*, `provenance{}`*, `inputs{}`, `step` |
| `knowledge` | none |
| `link_knowledge` | `source`*, `target`*, `relation`* |

`verified: true` is rejected for workers. `step` may only name your own step.

Artifacts are addressed by SHA-256, synced before their database reference commits,
and checked on retrieval. `reuse_artifact` returns a verified prior result only
when the `inputs` fingerprint matches exactly.

## Delegation and workflow

| Operation | Arguments |
| --- | --- |
| `delegate_task` | `id`*, `objective`*, `template`, `peer`, `bundles[]` |
| `list_children` | none |
| `integrate_child` | `child`*, `validation[]`* |
| `environments` | none |
| `propose_steps` | `steps`* — planner role only; inserted before the planning step's pending successors |

## Native tools

A worker running on Horde's own Tuara executor has these tools in addition to the
coordination operations:

| Tool | Arguments | Notes |
| --- | --- | --- |
| `read_file` | `path` | UTF-8 file in your worktree |
| `search` | `pattern` | ripgrep over the repository |
| `write_file` | `path`, `content` | Complete file; requires an exclusive claim |
| `apply_patch` | `patch` | Unified diff; every changed path is validated against claims |
| `command` | `argv[]` | Runs in the workspace; environment excludes provider credentials; resulting changes are checked against claims |

File tools reject path traversal and symlink traversal. `command` is removed
entirely when `allow_commands = false`.

A Codex or Claude Code harness worker uses its own file tools under its own
sandboxing, with the same coordination operations supplied as MCP tools. Those
edits cannot all be checked before they happen, so they are inspected before
integration and out-of-scope results are held for reconciliation rather than
merged.
