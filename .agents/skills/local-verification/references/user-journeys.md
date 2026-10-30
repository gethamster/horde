# User journeys across running services

Choose the existing journey that can demonstrate the affected behavior. Do not
invent a complete new smoke framework or require every journey below for an
unrelated change. Use the actual owner interfaces and preserve their permission
boundaries rather than modifying a downstream database to simulate success.

## Select evidence for the claim

| Claim | Relevant check |
| --- | --- |
| The installation starts from its declared inputs. | Start an isolated installation through its supported entrypoint with the documented configuration and disposable initial state. |
| Authentication and authorization work. | Exercise the intended identity flow and an applicable access boundary; preserve origin, audience, and account scope. |
| An edit is durable. | Perform the edit, observe its saved result, and reload or reconnect through the owning interface. |
| An asynchronous operation completes. | Follow its returned operation identity through admission to the expected result; a submitted job is not a completion. |
| Restart or upgrade preserves behavior. | Preserve the existing state, use the supported lifecycle procedure, and verify the affected record or operation afterward. |
| Concurrent clients behave correctly. | Exercise the relevant overlap and check the expected conflict, version, or ownership behavior. |
| The workflow recovers from prior activity. | Reproduce the meaningful sequence with retained evidence; a clean restart alone does not qualify the original sequence. |

Use synthetic fixtures where possible, while making the executed path explicit.
A test-only sign-in can qualify its own path but not a real identity provider.
A mocked response can qualify client behavior but not the live downstream service.
For real model or external-service calls, keep the approved identity, provider,
input constraints, and spending limits; obtain authorization if those calls are
outside the existing task scope.

## Keep fixtures and results honest

Seed accounts and records through the existing fixture or enrollment procedure.
Use identities isolated to the test’s environment and preserve the intended access
scope. Do not grant broader permissions or switch to a different provider merely
to make the journey pass. When setup behavior changes, update the owning fixtures
and local runbook together.

Inspect the result that matters to the user: a visible response, a saved record,
a completed operation, or a correct denial where denial is the requirement.
Activity counters, accepted requests, or a healthy dependency do not by themselves
show that the requested operation succeeded. An expected refusal must not stand
in for a serviceable request that should have completed.

Record the returned references, relevant revisions, observed outcome, and important
unverified parts. Keep private content and credentials out of reports. Distinguish
fresh-install, existing-state, simulated, live, and production evidence.

## Preserve failures and clean up ownership

Capture the relevant sequence, operation IDs, service revisions, and bounded
diagnostics before changing a failing environment. Reconcile uncertain writes
through their original identity before replaying them. See
[Incident recovery](../../incident-recovery/references/recovery.md)
for uncertain submissions and partial results.

Delete only task-owned test records when cleanup is part of the approved test.
Stop task-owned long-running jobs and services through the supported mechanism.
Do not stop the shared environment, reset its data, or remove another owner’s
fixtures as incidental cleanup. Report uncertain cleanup outcomes rather than
repeating destructive actions blindly.
