---
name: horde-review
description: Review Horde task results and proposed workflow-skill changes against the user's scope, execution contract, and recorded validation.
---

Review the actual result against the original acceptance criteria. Check which
runtime and capability ran each relevant attempt, what was tested, and what
remains uncertain. A successful remote report alone does not establish that its
changes integrate cleanly with another worker's result.

Use an independent reviewer when the task's risk and approved capability pool
justify it. Choose among advertised, permitted capabilities; do not assume every
model named in the pool must participate. Report concrete defects with evidence
and distinguish them from optional improvements.

Apply the inherited `horde-model-selection` guidance to review effort; use
`read_skill` if the skill is available but not already in context. Consequential
or uncertain changes may justify a stronger reasoning model. Deterministic
validation can be sufficient for a routine change with decisive acceptance
checks. Model tier alone is not evidence that the result is correct.

Workers can review their pinned guidance through `list_skills` and `read_skill`
and send proposed improvements to the caller. Worker credentials cannot edit
project policy. For durable workflow changes, the caller uses `skill_inspect`
to compare the shipped or
configured baseline with the effective project override. Review the exact
`skill_propose` diff and its scope before asking for explicit acceptance. Only
then call `skill_apply` with that proposal ID and `accepted:true`. A stale
proposal requires a fresh comparison, not an automatic overwrite.

`skill_history` shows applied revisions. `skill_rollback` creates another proposal;
review and accept it through the same apply step. Existing tasks keep their pinned
skill versions, including after a rollback. Describe that limit when deciding
whether a fresh task is needed.
