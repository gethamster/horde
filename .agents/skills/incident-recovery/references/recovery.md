# Recovery and concurrency

Use this reference when an unresolved operation may already have committed, or
conflicting state and overlapping work prevent safe continuation. Apply only the
relevant recovery step. Ordinary service calls and straightforward bug fixes do
not require this procedure or a new recovery subsystem.

## Establish what happened before retrying

| Observed state | Next step |
| --- | --- |
| Work is known not to have been submitted. | Submit through the existing authorized path if its budget and deadline remain valid. |
| The owner definitely refused the request without committing an effect. | Address the documented cause; retry only when the service contract and authority allow it. |
| Submission may have committed, but its response was lost. | Reconcile through the original operation identity, status endpoint, receipt, or owner record before considering another submission. |
| A response stream has already delivered output. | Report the interruption or use a documented continuation protocol; never silently restart the stream. |
| The owner confirms completion. | Reuse the confirmed result and verify the remaining requested outcome without replaying the effect. |

Preserve operation IDs, receipts, reservations, and other commitments while the
outcome remains uncertain. Cancellation does not prove that an in-flight effect
was rolled back. If reconciliation is unavailable, keep the state explicitly
unknown and report the missing capability rather than guessing success or failure.

## Preserve ownership during overlap

Bind requests and their effects to the correct tenant, project, user, source
revision, and resource owner. Concurrent callers must not overwrite each other’s
operation identity or reuse an unrelated receipt. Use the repository’s existing
atomic transitions or concurrency controls for authority-dependent state changes.

Keep checks at the owning authority. Avoid repeating unchanged validation, while
rechecking permissions or other facts that may have changed before a consequential
transition. Local cached evidence cannot replace a required current authorization.

Preserve usable capacity and commitments between operations. Reconciliation or
forecasting should not block otherwise serviceable requests solely to restore a
preferred internal state. Reject malformed requests at their boundary; one invalid
request or unsuitable probe is not evidence that an independent healthy service
must be suspended.

## Make recovery possible

For each relevant stop condition, identify who can resume work, the evidence
needed, the resources still available, and the applicable budget or deadline.
Recovery should not depend on new traffic or on a resource whose exhaustion caused
the stop. Keep a failed dependency or background operation from unnecessarily
stopping independent work.

Budget queueing, database work, upstream time, and recovery within the operation’s
latency contract. Measure contention and redundant work before adding retries,
caches, concurrency, or longer deadlines. Recovery shares the original limits;
it does not create permission, spending capacity, or a fresh deadline.

## Verify the affected failure

Preserve the failing environment and relevant evidence before cleanup. Reproduce
its important state, overlap, or interruption with authorized synthetic inputs.
A successful fresh installation establishes fresh-start behavior; it does not
resolve a failure that depends on an existing installation’s state.

Verify both the recovered result and the affected ownership, data, and resource
invariants. Keep unrelated provider failures separate, and label environmental
substitutions so their evidence is not mistaken for the original operation.
