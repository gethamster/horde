# Shared service environments

Coordinate access to shared running services through one designated owner.
This applies to native processes, service managers, sandboxes, and other supported
runtimes; it does not assume a container-based stack.

## Establish the owner and target

Record the environment owner, relevant service or artifact revisions, intended
configuration, shared state, and active consumers. Keep sensitive values in the
existing private configuration rather than the handoff message.

Contributors request the version or operation they need from that owner. They do
not independently restart services, replace state, claim occupied ports, or launch
a competing instance. If ownership is unclear, resolve it before mutating the
shared environment; independent work may continue elsewhere within scope.

## Schedule changes without invalidating evidence

The owner coordinates the sequence of updates and checks. Keep dependent tests
on the intended revision and configuration. A restart or configuration replacement
can invalidate another contributor’s evidence even when the service remains healthy.

State which revision actually serves a check. A locally rebuilt binary or merged
commit does not prove adoption. When a change requires restart or reload, use the
supported lifecycle operation and verify the affected behavior afterward.

Concurrent read-only checks may share the environment when they do not interfere.
Checks that mutate common data or configuration need isolated identities/state
or an agreed sequence. Keep destructive fresh-install tests in an isolated
installation; do not reset shared state to obtain a clean result.

## Keep the owner available through handoff

Reuse the existing environment owner rather than creating a new coordinator for
each test. Keep services available while active consumers need them. Before the
owner exits, transfer lifecycle responsibility and the relevant handles to a
continuing owner, or record why ownership cannot yet be transferred.

Follow [Local Verification](../../local-verification/SKILL.md) when the assignment
needs detailed verification of running service behavior. For resources whose
remaining use is complete, pass their ownership and usage status to
[Workspace Cleanup](../../workspace-cleanup/SKILL.md); do not duplicate its
preservation or removal rules here.
