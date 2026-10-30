# Incident evidence map

Use this map to connect a failed user operation to the responsible running
services, source changes, build artifacts, and observed recovery. Select only
the evidence needed for the incident; use the owning repository’s existing
runbooks and authorized diagnostic interfaces rather than inventing a new
collector or assuming a particular cloud, log service, or Git host.

| Evidence | Find it through | Establish |
| --- | --- | --- |
| Original operation | The returned request, trace, job, or error reference and its owning record. | Expected versus observed behavior, time, relevant scope, and original operation identity. |
| Cross-service path | Correlated logs, traces, or operation records at the affected service boundaries. | Which component handled or rejected the operation, and where evidence is missing. |
| Running revision | The service’s supported runtime identity, deployment record, or process/artifact diagnostics. | Which code and configuration actually served the operation, not merely what was merged or built. |
| Source and review | The owning repository’s commit, diff, and review records. | The implementation and assumptions connected to the running artifact or proposed repair. |
| Build and artifact | The owning build record, resolved inputs, output identity, and relevant checks. | Which source produced the artifact and what has been verified about that output. |
| Follow-up operation | A successful affected operation on the repaired target, using authorized inputs and the intended constraints. | Whether the original behavior is restored; keep new operation identity distinct from the failed attempt. |
| Recurrence and latency | Bounded observations with timestamps, traffic conditions, and relevant metrics. | What the observation window supports and what remains unknown. |

## Collect and correlate

Start with the failed operation and follow its path only as far as the evidence
requires. Keep tenant, project, account, provider, model, or other boundaries when
they affect the claim. Use actual returned references; do not invent a trace,
revision, artifact identifier, or source link to complete the table.

Correlate evidence by explicit identifiers and compatible times. Similar error
messages are not proof of a common cause. Several alerts may describe one defect;
one message may also describe independent failures. Keep hypotheses separate
from observed facts and say which missing boundary prevents a conclusion.

Distinguish an upstream refusal or failure from internal queueing, timeouts,
authorization, and input validation. An internal timeout does not turn a valid
request into a malformed one. Reuse an existing investigation when its demonstrated
cause and scope match rather than restarting work for each alert.

## Verify the repair against the failure

Compare the original failure with a successful affected operation on the actual
repaired target. Preserve the relevant inputs, identity, and constraints. Use
authorized synthetic inputs when they can establish the claim. Reconcile an
uncertain original effect before any retry; collecting evidence does not authorize
replaying private requests or repeating a possibly committed action.

For a package or local-only repair, use the corresponding consumer or local
behavior instead of claiming a serving production revision exists. Follow
[Verification and release evidence](../../verified-delivery/references/verification.md) for the applicable endpoint,
checks, review, and artifact requirements.

Record the observation window and relevant traffic volume or conditions. A quiet
log, closed alert, changed severity, or low-traffic period does not prove recovery
under the failed workload. Distinguish delayed telemetry ingestion from the
operation directly observed, and report unavailable evidence as unknown.

## Preserve useful evidence safely

Retain the references and bounded evidence needed to support the conclusion and
continue unresolved work. Do not copy credentials, private user content, or raw
transcripts into a broader-audience report. Respect the owning record’s access
and retention rules. Source attribution alone does not authorize disclosure.

Report the established cause or current hypothesis, affected scope, source and
artifact identities, observed recovery, and remaining gaps. Keep built, tested,
published, serving, and behaviorally verified states distinct. Do not introduce
another approval gate or declare success merely because collection completed.
