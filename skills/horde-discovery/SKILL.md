---
name: horde-discovery
description: Inspect Horde runtime and model capabilities to identify valid execution choices and blockers before planning or delegation.
---

Read `runtime_capabilities` for the local runtime and requested workers. Resolve
human model names to the actual advertised capability IDs on each runtime. An ID
belongs to its runtime; identical labels on two machines do not prove identical
provider configuration. Report unavailable, stale, or ambiguous choices instead
of substituting a different endpoint.

Separate a user's allowed pool from the capability selected for one task. For
example, “Apollo may use Claude, Codex, or GLM 5.3” permits a choice among the
advertised matches. It does not require three calls or authorize another model.
Use `plan_execution` to resolve role pools and expose blockers. Preserve any
explicit user choice and the requested local versus remote placement.

Discovery is read-only. It does not enroll workers, grant execution, change
provider credentials, or edit persistent skills. Fresh capability evidence can
inform the caller's next decision; a running task retains its pinned contract.
