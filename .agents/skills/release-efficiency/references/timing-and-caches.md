# Timing and caches

Measure the pipeline that actually runs, using its existing logs and timing
records. Keep the measurement small enough to answer the observed performance
question; do not create an observability service for a single slow build.

## Separate waiting from work

Capture timestamps or durations for the stages present in the repository:

| Stage | Useful evidence |
| --- | --- |
| Queueing | When work became eligible, when it started, and which runner or resource it awaited. |
| Source and dependencies | Checkout, dependency acquisition, cache restore, and cache miss reasons. |
| Compilation or bundling | Actual compiler or bundler execution, including repeated targets. |
| Tests and review | Required checks, review waiting, and whether independent work overlapped. |
| Packaging and publication | Archive or image creation, upload, signing, and registry or repository completion. |
| Staging and promotion | Shared-environment waits, deployment, approval, and rollout. |
| Outcome verification | The check that establishes the requested package or service works. |

Use the repository’s actual delivery endpoints. A package release need not have
staging or a serving revision; a local-only request does not require publication.
Do not add durations of parallel steps as if they were all on the critical path.
Report elapsed total time alongside stage times so shifted work remains visible.

Compare runs with relevant inputs recorded: source and dependency revisions,
change class, target platform, runner resources, warm or cold caches, and required
checks. State the sample count and observation period. Report percentiles only
when the available sample supports them; a single fast run is not a reliable p95.
Distinguish successful, failed, cancelled, and retried runs rather than silently
removing slow attempts from the sample.

## Verify cache behavior before depending on it

Identify where cache data lives and whether that storage survives the runner’s
lifetime. A local cache mount on one worker does not imply reuse on a fresh
worker. Check actual cache-hit evidence and dependency work avoided.

Keep cache keys sensitive to the inputs that affect correctness, including
relevant lockfiles, toolchain versions, platform, feature flags, and configuration.
Use the repository’s existing cache mechanism and trust boundaries. Do not share
credentials, private build output, or executable caches across untrusted jobs.

Measure restore, upload, and invalidation costs against the work saved. Test the
relevant cold-cache or missing-cache path so correctness does not depend on a
warm runner. Prefer eliminating duplicate compilation or acquisition when it
removes the bottleneck without adding another cache or service.

## Act on the measured constraint

If queueing dominates, investigate capacity and scheduling before editing the
compiler step. If compilation dominates, inspect repeated work and target inputs.
If shared staging dominates, coordinate its owner and remove unnecessary holds.
More coding agents do not resolve a saturated publication or staging resource.

Keep optimizations within existing authority and resource budgets. Raising a
timeout only changes when failure is reported; it does not remove the delay.
State the baseline, comparable result, and any throughput or resource-cost tradeoff.
