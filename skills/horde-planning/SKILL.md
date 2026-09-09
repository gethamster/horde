---
name: horde-planning
description: Turn a requested outcome and permitted runtime/model pools into bounded Horde tasks with explicit execution choices and acceptance criteria.
---

Keep the caller in control of task decomposition and model choice. Start with
the requested outcome, repository, constraints, and current `runtime_capabilities`.
Use `plan_execution` to resolve role pools. For example:

```json
{"roles":{"thinking":{"runtime":"local","models":["codex","astra"]},"delivery":{"runtime":"apollo","models":["claude","codex","glm 5.3"]}}}
```

Apply the inherited `horde-model-selection` guidance to match task difficulty and
verification needs to an allowed model. If its instructions are not already in
context, use `read_skill` when it is in the pinned catalog. Keep consequential
planning with a suitable reasoning model; give settled implementation and narrow,
easily checked work appropriately smaller assignments. A capability advertisement
alone does not establish model quality or cost.

The response identifies real advertised choices, blockers, and a combined
`allowed` pool. Translate the user's request into these calls; explain choices
in plain language without asking the user to assemble JSON. Choose a capability
for each bounded task according to its needs; mentioning several models does not
require using all of them. Explain a meaningful tradeoff when it affects the
user's requested cost, runtime placement, or validation. Do not invent capability
IDs, silently expand an allowed pool, or treat inventory visibility as a grant.

Submit or delegate with an execution contract shaped as
`{"allowed":[{"runtime":"...","capabilities":["..."]}],"selected":{"runtime":"...","capability":"..."}}`.
Use a stable `request_id` for `submit_task` and a stable child `id` for
`delegate_task`. After a timeout, retry that identifier with the original intent;
a fresh identifier could start duplicate work.

When a root task will create children across role pools, give the root the union
of those approved pools in `execution.allowed`. Keep its `selected` capability
on the local thinking runtime. Record the role constraints in the task context:
local capabilities perform thinking, and Apollo capabilities perform delivery.
Each delivery child narrows `allowed` to the Apollo delivery pool and selects an
Apollo capability. A root restricted to its local pool cannot later grant a child
access to Apollo. The combined pool permits the planned child delegation; it does
not authorize ignoring the role constraints.

Give independent workers distinct responsibility, required context, and concrete
acceptance criteria. Preserve dependencies when one result is needed by another.
Results that change a shared repository need integration and combined validation.

Workers inspect their pinned guidance with `list_skills` and `read_skill`, then
send suggested persistent improvements to the caller. The caller uses
`skill_inspect` and `skill_propose` to present a project diff, and calls
`skill_apply` only after explicit acceptance. Worker credentials cannot call
these administrative tools. Submitted tasks and their children retain their
pinned bundles, so later approved edits affect new submissions.
