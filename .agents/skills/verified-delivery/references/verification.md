# Checks and eval decisions

Choose the shortest useful path from the reported problem to a demonstrated
fix. Keep the repository’s required checks; do not add reassurance work around them.

## Pick the proof that answers the question

Use the existing reproduction, smoke test, or eval for the affected behavior.
For a deployed service, exercise it in the existing staging environment with
the intended input and identity. For a package, run its relevant consumer or
package smoke/evals through the established release path. For local-only work,
verify locally. For documentation-only work, check the changed content and links.

A new eval is worthwhile only when existing checks cannot establish the requested
behavior. It can be a small temporary scenario; it does not require a harness,
registry, benchmark suite, or permanent file. If required staging or another
mandatory gate is unavailable, say exactly which gate is blocked rather than
silently substituting a weaker environment and calling it passed.

## Decide pass or fail from evidence

State the expected behavior, run the relevant check, and compare its observed
result. A failed check needs a concrete mismatch, error, or unmet requirement.
A passing result needs evidence that the requested operation worked, not just
that a process was healthy or a job was accepted.

Do not lower the requirement, change the input or provider to hide a failure,
or relabel a failing eval. Optional improvements and hypothetical edge cases
outside the request do not turn a pass into a failure.

If a check fails, read its output and change something that addresses the cause
before retrying. Repeating an unchanged attempt without new evidence is not
progress. Repeat only checks affected by the repair or changed inputs. Reuse
valid results and artifacts; do not repeat a suite because a stage name changed.

## Retain only useful tests

A temporary script or reproduction can provide all the evidence needed for a
small bug fix. Remove it after verification when its continuing value is low.
Keep a permanent test when it protects important behavior, likely recurrence,
or a required contract without disproportionate maintenance.

Do not add tests that mirror implementation shape, assert deleted files are
absent without a behavioral reason, or preserve a one-off debugging procedure.
Do not delete useful existing tests, weaken assertions, or skip required gates
merely to make a run green. Test creation and retention are judgment calls, not
mandatory ceremony for every patch.

## Stop when the decision is made

After relevant smoke/evals and required gates pass, ship through the existing
workflow. Re-review or retest only for new changes, new failure evidence, or an
actual repository requirement. Keep required independent or human review, but
do not commission extra reviewers to debate a passing bounded repair.

Confirm delivery at the requested endpoint: local files for a local request,
a published version for a package, or the intended artifact/revision for a
service deployment. Do not add an open-ended observation window as a release
gate unless the requested incident or repository policy actually requires one.
