---
name: release-efficiency
description: >-
  Use when changing a build or release pipeline, diagnosing slow CI or publication,
  or reducing delivery time. Measure the actual bottleneck, improve scheduling and
  artifact reuse, and preserve required verification, approvals, and compatibility.
metadata:
  audience: "contributors"
---

# Release Efficiency

Improve the time needed to deliver verified work through the owning repository’s
existing pipeline. Follow [verified-delivery](../verified-delivery/SKILL.md) for
authorization, required checks, evidence reuse, and the requested completion
boundary. This skill adds pipeline diagnosis and optimization; it does not add a
release stage or require every coding task to tune CI.

## Find the actual bottleneck

Read the pipeline configuration, the version of any controller actually running,
and representative completed or failed runs. Trace the path from ready source to
the requested verified outcome, including queueing and manual or shared-resource
waits. A checkout is not proof that its release tooling is deployed.

Use [Timing and caches](references/timing-and-caches.md) to separate waiting from
execution, compare equivalent workloads, and check whether caching works in the
actual runner environment. Read the slow step’s log before changing it. Report
missing timings as unknown and use agreed repository targets rather than
inventing universal delivery deadlines.

## Shorten the path without weakening it

Remove demonstrated duplicate work before adding caches, services, workers, or
longer timeouts. Parallelize independent builds and review when the saved time
exceeds contention and coordination costs. Serialize real dependencies and
mutations of shared staging environments; keep one owner for those environments.

Preserve mandatory tests, protections, human decisions, and security boundaries.
Reuse completed builds and still-valid evidence under the owning policy. A
cancelled or superseded deployment does not invalidate a successfully built
artifact, but changed inputs can invalidate the artifact’s qualification.

Ship ready work after any required review before expanding a historical investigation. Keep
unrelated failures scoped to their affected operations unless they block a
required dependency or gate. Do not classify an unfinished or skipped required
check as an optimization.

## Keep the pipeline compatible

Use [Pipeline compatibility](references/pipeline-compatibility.md) when changing
manifests, dispatchers, artifact handling, or staging coordination. Deploy support
for a new input format before depending on it. Preserve rollback and existing
qualified artifacts during the transition.

## Verify the improvement

Run the relevant required checks and observe the affected pipeline path after the
change. Compare it with a stated baseline using similar workloads and runner
conditions. Account for work moved into another stage, retries, or resource costs;
a shorter build is not an improvement if total verified delivery is slower.

Report the bottleneck, change, measured stage and total times, workload conditions,
and remaining uncertainty. Retain verification of the artifact or operation at
the requested endpoint. An accepted pipeline edit or queued build does not prove
a speed improvement.

The [adaptation record](references/source.md) identifies the original guidance.
