---
name: horde-model-selection
description: Choose permitted Horde models for planning, implementation, and bounded fast work using task difficulty, verification needs, and evidence about the available models.
---

Choose a model for the work being assigned. Treat the user's named models as an
allowed pool unless they explicitly assign a particular model to a task. Preserve
machine placement and role constraints through every child; access to a model
does not authorize its use outside that scope.

Use these defaults when the available evidence supports the model's suitability:

| Work | Default choice |
| --- | --- |
| Architecture, uncertain requirements, consequential planning, or debugging with competing explanations | A model with strong reasoning and relevant problem-solving evidence. |
| Bounded implementation with a settled design and useful tests | A competent mid-tier model that can complete and verify the change. |
| Narrow extraction, classification, or routine transformations with cheap, decisive checks | A tiny or fast model, if a deterministic tool cannot already do the job reliably. |

Task risk can outweigh apparent size. A short authentication change may need
stronger reasoning than a large mechanical rename. Reserve stronger independent
review for uncertainty or failure costs that justify it within the approved pool.
Routine changes with decisive checks do not require an expensive review gate.

Resolve actual runtime and capability IDs through `runtime_capabilities` and
`plan_execution`. Inventory establishes executability and readiness; it does not
establish a model's quality tier, speed, or price. Map the available models to
these roles using explicit project guidance, verified provider documentation, or
observed task results. Keep unsupported comparisons unknown. If that uncertainty
materially affects the assignment, explain it and seek the missing evidence or
direction rather than inventing a ranking.

Keep model identity separate from its execution harness and reasoning settings.
Codex or Claude may name a harness or a model choice in conversation; resolve the
advertised pair before dispatch. A reasoning-effort setting changes how a
supported model runs, not its identity or quality tier. Use such a setting only
when the selected executor exposes it; do not invent a field or assume another
provider's setting works. For example, a documented high effort option may suit
a difficult investigation while the same model's lower effort option may suffice
for a bounded follow-up.

Include the chosen runtime/capability, scope, relevant context, and acceptance
checks in the delegation brief. Carry its permitted pool in the execution
contract. Briefly explain material tradeoffs, then proceed within the existing
authorization. There is no requirement to use every permitted model.

Optimize the time and cost of a verified result, including context transfer,
parallel coordination, retries, and integration. Split independent work only
when the savings justify that overhead. Repeated weak attempts can cost more
than one capable attempt. When a worker exposes unresolved reasoning or
verification gaps, return them to the parent for a bounded replan. Refresh stale
capabilities; do not silently substitute a model or expand the allowed pool.
Preserve the original task identifier and intent when retrying an uncertain
dispatch. Reconcile its recorded state before creating replacement work.

Use observed outcomes to suggest narrow improvements to this policy. Workers
report those suggestions to their parent. The caller presents a project skill
proposal for discussion and explicit acceptance before applying it. Accepted
changes affect new submissions; running tasks and descendants retain their
pinned guidance.
