---
name: verified-delivery
description: >-
  Always apply to contributor changes. Solve the requested problem, run the
  relevant staging smoke checks and evals, decide pass or fail, and ship the fix.
  Avoid speculative engineering, unnecessary permanent tests, and repeated
  analysis or review after the evidence is sufficient.
metadata:
  always-run: "true"
  audience: "contributors"
---

# Verified Delivery

Solve the problem and ship the fix. Overthinking, speculative redesign, duplicate
checks, and repeated reviews consume time and money without improving delivery.
Use judgment to finish the requested work, not to invent more work.

1. **Find the concrete problem.** Read enough source and evidence to understand
   the reported failure or requested behavior. Use an existing reproduction when
   available. Do not turn a bounded bug into a repository-wide investigation.
2. **Make the smallest complete fix.** Preserve the requested behavior and use
   the existing implementation and delivery path. Do not add an architecture,
   framework, abstraction, or unrelated refactor to make a local repair elegant.
3. **Prove the fix efficiently.** A test is a tool, not an automatic deliverable.
   Use a temporary reproduction or focused test when it helps, then remove it
   if it adds no continuing value. Keep a regression only when it protects a
   meaningful contract or likely recurrence at reasonable maintenance cost.
   Preserve useful existing tests and required repository checks.
4. **Smoke-test and evaluate.** Exercise the affected operation in the existing
   staging environment and run the relevant evals that exist. Compare observed
   behavior with the requested result. Do not build a new test framework or full
   suite for a small fix. Documentation-only work needs content and link checks;
   honor an explicit local-only or investigation-only request.
5. **Decide.** Pass when the requested behavior works, relevant smoke checks and
   evals pass, and required repository gates are satisfied. Fail for an observed
   defect or unmet requirement, not a speculative improvement. On failure, fix
   the demonstrated cause and rerun the affected check. Once passed, stop
   evaluating unchanged work and proceed to delivery.
6. **Ship.** Use the repository’s established authorized commit, review, release,
   and deployment path. Reuse the tested artifact where supported and confirm
   the intended version reached the requested destination. Do not add another
   review round, approval request, build, or test stage merely for reassurance.
   Retain mandatory human approvals and repository protections.

Use [Checks and eval decisions](references/verification.md) when choosing proof,
retaining a test, or deciding whether another check is warranted. If a normal
fix is genuinely blocked by an unresolved incident or uncertain committed effect,
use [incident-recovery](../incident-recovery/SKILL.md); do not load its full
investigation procedure for every bug.

Continue within existing authorization. A missing required staging environment,
failed mandatory gate, or missing permission is a specific blocker to report,
not permission to fake a pass or bypass a protection. Complete independent work
while it is resolved. Do not re-ask for authority already granted.

Report the fix, the smoke/eval result, and the shipped revision or exact blocker.
Keep it short. Follow [workspace-cleanup](../workspace-cleanup/SKILL.md) for task
resources; cleanup is not a new engineering project.

[Source and adaptation history](references/source.md).
