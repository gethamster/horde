---
name: horde-delegation
description: Delegate bounded Horde work to an approved runtime and model while preserving task authority, skill pins, and recoverable results.
---

Delegate work that has a clear owner and independently reviewable result. Include
the objective, repository context, scope, dependencies, acceptance criteria, and
selected advertised capability. Use `delegate_task.execution` or
`submit_task.execution` to carry the allowed pool and one selected runtime and
capability. Keep the selected pair inside the parent's permitted pool. A root
that needs local thinking and remote delivery must include both approved role
pools; its selected capability still performs local thinking. Narrow each delivery
child to its delivery pool, and preserve those role constraints in task context.

Use the inherited `horde-model-selection` guidance when choosing the worker's
model; call `read_skill` if it is available but not already in context. Match the
assignment's uncertainty and acceptance checks to demonstrated model ability.
Give a smaller model a bounded brief with decisive checks, and send unresolved
reasoning gaps back to the parent rather than silently widening the assignment.

For “think locally, implement on Apollo,” keep planning context with the caller
and send Apollo the bounded implementation contract. Another model in the same
allowed pool is an option, not an automatic fallback or a reason to duplicate
work. Re-resolve stale or missing capability evidence before making a new choice.

Inherit the parent's pinned skill catalog or explicitly narrow it with `skills`.
Do not replace a child's skills with current files from the receiver. A worker's
report is provisional until the parent checks the result and any combined tests.
Assign a stable `request_id` to `submit_task` and stable `id` to `delegate_task`.
After an interruption, retry that identifier with its original intent or inspect
the durable task. A new identifier could repeat work that already executed.

A worker may send a project skill improvement to the caller for a reviewable
proposal; worker credentials cannot call the administrative skill tools.
Task completion does not authorize persistent policy changes or expand remote
execution grants. Keep private credentials out of delegated context.
