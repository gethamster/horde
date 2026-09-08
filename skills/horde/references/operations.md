# Operation reference

Every operation below is available two ways with identical arguments and results:

```sh
horde call OPERATION '{"json":"object"}'
```

or as an MCP tool on the `horde mcp` bridge. Arguments must be a JSON object.
Results are JSON.

Notation: `*` marks a required field. `task` identifies the task and is required
for every task-scoped operation when you call it from the CLI or the
personal-agent bridge. A worker token supplies `task` and `worker` itself and
rejects any attempt to name a different one.

## CLI shortcuts

| Command | Operation |
| --- | --- |
| `horde submit OBJECTIVE --repo P --template T` | `submit_task` |
| `horde list` | `list_tasks` |
| `horde inspect ID` | `inspect` |
| `horde events ID --after N` | `events` |
| `horde metrics ID` | `metrics` |
| `horde cancel ID` | `cancel` |
| `horde resume ID` | `resume` |
| `horde answer ID QID ANSWER` | `answer_question` |
| `horde config get/set concurrency N` | `runtime_config_get` / `runtime_config_set` |
| `horde usage` | `account_status` |
| `horde runtime status/drain/resume/list/inspect/create/...` | `runtime_*` |

Other commands have no operation equivalent: `start`, `stop`, `daemon`, `mcp`,
`doctor`, `validate`, `config` (print), `service`, `update`, `network`.

## Task lifecycle

| Operation | Arguments | Notes |
| --- | --- | --- |
| `submit_task` | `objective`*, `repo`*, `template`, `context[]` | Returns `{"id":...}`. `repo` is an absolute path. `context` entries are context records. |
| `list_tasks` | none | |
| `inspect` | `task`* | Steps, attempts, workers, questions, integration state. |
| `events` | `task`*, `after`, `consumer` | Ordered activity events. |
| `ack_events` | `task`*, `consumer`*, `seq`* | Durable, monotonic per-consumer receipt. Does not answer questions. |
| `metrics` | `task`* | Usage, cost, latency, retries, coordination counts, `children` tree. |
| `cancel` | `task`* | Kills active process groups; cancels the owned subtree. |
| `resume` | `task`* | Interrupted processes must be reconciled first. |
| `add_steps` | `task`*, `steps`* | Appends a validated workflow revision. |

## Questions

| Operation | Arguments | Worker |
| --- | --- | --- |
| `request_question` | `task`*, `worker`, `question`*, `id`, `evidence`, `recommendation`, `human_only` | yes |
| `pending_questions` | `task`* | yes |
| `answer_question` | `task`*, `question`*, `answer`*, `worker`, `human` | yes |
| `escalate_question` | `task`*, `question`*, `commentary`, `worker` | yes |

## Delegation

| Operation | Arguments | Worker |
| --- | --- | --- |
| `delegate_task` | `task`*, `id`*, `objective`*, `template`, `peer`, `bundles[]`, `worker` | yes |
| `list_children` | `task`* | yes |
| `integrate_child` | `task`*, `child`*, `validation[]`*, `worker` | yes |
| `read_context` | `task`*, `after`, `limit` | yes |
| `update_context` | `task`*, `content`*, `provenance`*, `kind`, `id`, `mandatory`, `supersedes[]` | no (root caller only) |

## Coordination

| Operation | Arguments | Worker |
| --- | --- | --- |
| `register_worker` | `task`*, `step` | no |
| `register_workspace` | `task`*, `worker`, `path`*, `branch`*, `base`* | yes |
| `list_workers` | `task`* | yes |
| `set_worker_status` | `task`*, `worker`, `status`* (`idle`/`working`/`blocked`/`stopped`) | yes |
| `claim_paths` | `task`*, `worker`, `paths[]`* | yes |
| `transfer_claim` | `task`*, `worker`, `to`*, `path`* | yes |
| `release_claims` | `task`*, `worker`* | no |
| `reconcile_worker` | `task`*, `worker`* | no |
| `send_message` | `task`*, `worker`, `id`*, `destination`*, `body`*, `refs{}`, `actionable` | yes |
| `read_messages` | `task`*, `worker`, `after`, `limit` | yes |
| `acknowledge_messages` | `task`*, `worker`, `ids[]`* | yes |
| `join_channel` | `task`*, `worker`, `channel`* | yes |
| `integrate` | `task`*, `worker`, `validation[]` | no |
| `propose_steps` | `task`*, `worker`, `steps`* | yes (planner) |

## Artifacts and knowledge

| Operation | Arguments | Worker |
| --- | --- | --- |
| `put_artifact` | `task`*, `name`*, `content`*, `inputs{}`, `verified`, `worker`, `step` | yes, but cannot set `verified: true` |
| `get_artifact` | `task`*, `hash`* | yes |
| `reuse_artifact` | `task`*, `name`*, `inputs{}`* | yes |
| `add_knowledge` | `task`*, `kind`*, `content`*, `provenance{}`*, `inputs{}`, `verified`, `step` | yes, but cannot set `verified: true` |
| `knowledge` | `task`* | yes |
| `link_knowledge` | `task`*, `source`*, `target`*, `relation`* | yes |

## Environments and secrets

| Operation | Arguments | Worker |
| --- | --- | --- |
| `environments` | `task`* | yes |
| `refresh_bundles` | `task`* | no |

## Runtime and fleet management

None of these are available to a worker token.

| Operation | Arguments |
| --- | --- |
| `runtime_config_get` | none |
| `runtime_config_set` | `concurrency`* |
| `runtime_status` | none |
| `runtime_drain` / `runtime_resume` | none |
| `runtime_list` | none |
| `runtime_inspect` | `id`* |
| `runtime_create` | `id`*, `profile`*, `request_id`* |
| `runtime_destroy` / `runtime_restart` / `runtime_start` / `runtime_stop` | `id`*, `request_id`* |
| `runtime_update` | `id`*, `request_id`*, `version`* |
| `runtime_reconcile` | `id`*, `request_id`*, `resource`* |
| `runtime_updates_resume` | none |
| `account_status` | none |
| `account_observe` | `account`*, `provider`*, `window`*, `observed_at`*, `source`*, `used_percent`, `reset_at` |
| `management_events` | `after` |
| `management_ack` | `consumer`*, `seq`* |

## Worker token scope

A worker token may call exactly these, and nothing else:

`integrate_child`, `environments`, `delegate_task`, `list_children`,
`read_context`, `pending_questions`, `escalate_question`, `answer_question`,
`propose_steps`, `request_question`, `send_message`, `read_messages`,
`acknowledge_messages`, `list_workers`, `set_worker_status`, `join_channel`,
`claim_paths`, `transfer_claim`, `register_workspace`, `put_artifact`,
`get_artifact`, `reuse_artifact`, `add_knowledge`, `knowledge`, `link_knowledge`.

Additional worker restrictions, enforced at the RPC layer:

- `task` and `worker` are forced to the token's own identity.
- A worker cannot attribute work to another worker's step.
- A worker cannot set `verified: true`. Workers report evidence; verification is
  an authoritative runtime decision.
- Arguments starting with `_` are rejected as reserved.

These are scope limits at the RPC layer, not process isolation. A worker token
does not protect against a malicious local process running as the same user.
