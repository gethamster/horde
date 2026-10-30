# Ownership and handoffs

Use a small, explicit assignment instead of relying on the worker’s copy of a
long conversation. Preserve the user’s scope and the repository’s authority
boundaries in every delegation.

## Assign an outcome and a write scope

Provide the worker with:

- The requested result and the evidence that would establish it.
- The repository, relevant source or base revision, and existing task or patch.
- Owned files, modules, or another clear boundary for writes.
- Dependencies, shared interfaces, active owners, and integration order.
- Applicable constraints, including data access, provider usage, and publication.
- The expected return: tested changes or findings, exact references, and blockers.

Tell editing workers that others are working in the repository and that they
must preserve those changes. Use isolated worktrees or checkouts. If work must
touch shared files, assign one owner or land the shared change before dependent
work. Do not let several workers independently rewrite the same contract.

Delegate only within the available tools and the user’s authorization. A parent
cannot grant broader credentials, spending, external communication, or deployment
rights by writing them into a worker prompt. Pass necessary context with its
source and audience restrictions; do not circulate private transcripts or secrets
as a substitute for a bounded handoff.

## Keep integration owned

The integration owner coordinates shared schemas, interfaces, configuration,
migration ordering, and release inputs across the affected components. Workers
return their exact commit or artifact identities and relevant validation.

Inspect those results before treating them as integrated. Do not infer that a
successful worker run proves combined behavior, or that a merge updates running
services. Resolve shared-file conflicts through the assigned owner and the
repository’s existing integration process.

Use the actual target branch and source identity when refreshing a candidate.
A stale base may explain a mergeability wait; do not merely wait for it to fix
itself. Preserve applicable evidence, but revisit tests and decisions whose inputs
or integration assumptions changed. Do not force-push a shared or protected
branch to avoid coordination.

## Transfer the next action

A useful handoff identifies the outcome, completed changes and references,
validation already performed, active work and its handles, unresolved findings,
and the next owner’s concrete action. Include the location and ownership of
resources the receiving work still needs.

Distinguish planned work, admitted work, running work, completed worker output,
integrated source, published artifacts, and verified operation. Preserve actual
status rather than translating every successful tool response into completion.

When a worker stalls or fails, inspect the existing operation before retrying or
reassigning. Reconcile uncertain external effects and preserve partial results.
Cancel or supersede only the intended task within current authority, and report
what actually stopped. Transfer or retain active resource ownership before
applying the shared cleanup procedure.
