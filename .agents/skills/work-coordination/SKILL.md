---
name: work-coordination
description: >-
  Use when planning or supervising work across contributors, coordinating parallel
  editing, handing work between agents, or sharing a running service environment.
  Assign clear ownership, isolate edits, preserve handoffs, and integrate through
  the repository's existing workflow.
metadata:
  audience: "contributors"
---

# Work Coordination

Coordinate people and coding agents around the requested outcome. Follow
[verified-delivery](../verified-delivery/SKILL.md) for authority, verification,
and completion, and [workspace-cleanup](../workspace-cleanup/SKILL.md) for
resource preservation and retirement. This skill owns assignments and handoffs,
not another cleanup or release procedure. A small fix can stay with one owner;
this skill does not require delegation, a planning document, or independent review.

## Assign coherent work

Inspect existing investigations, patches, and active owners before dispatching
new work. Assign one owner per distinct demonstrated cause or coherent outcome.
Split independent work when it shortens delivery after coordination costs; do
not create a chain of coordinators for work one owner can complete directly.

Give each assignment its outcome, repository and base revision, owned files or
modules, required inputs, relevant constraints, and evidence of completion.
Keep an integration owner responsible for shared contracts, ordering, validation,
and delivery. A worker finishing its assignment does not finish integration.

Use [Ownership and handoffs](references/ownership-and-handoffs.md) when work
crosses contributor or repository boundaries. Give editing workers isolated
worktrees or checkouts and assign shared files to one owner. Read-only reviewers
may inspect the committed patch without modifying the author’s checkout. Never
reset another contributor’s tree to remove apparent contamination.

## Supervise the actual work

Retain the returned task, operation, branch, worktree, and artifact identities.
Check actual status at meaningful changes instead of waiting indefinitely for a
message or dispatching a duplicate job. Preserve completed results if another
part fails, and keep independent authorized work moving.

Refresh a stale candidate against its actual target branch through the owning
repository’s workflow. Resolve conflicts without discarding unrelated changes.
Revalidate affected behavior and retain unchanged evidence where justified.
Rebase or rewrite only where permitted; a force-with-lease option does not grant
permission to rewrite someone else’s or protected history.

## Coordinate shared environments

Use [Shared service environments](references/shared-environments.md) when
contributors need the same running services, ports, state, or verification target.
One designated owner controls lifecycle changes. Reuse that owner rather than
starting a competing environment or spawning another coordinator for a check.

Coordinate builds and independent review when useful, while preserving required
review-before-publish ordering. More coding agents do not resolve a saturated
staging environment, build queue, or publication lock; address the actual
constraint through the existing owner and permitted capacity.

## Hand off and finish

Make completed work, unresolved findings, current handles, and the next action
available to the receiving owner. Transfer responsibility for shared services
before the current owner exits if consumers still need them. A handoff must not
silently abandon active work or claim an unverified result is complete.

After integration or handoff, apply
[workspace-cleanup](../workspace-cleanup/SKILL.md) to the resources no longer
needed. Supply its owner and remaining-use information; let that skill govern
preservation, eligibility, removal, and verification.

The [adaptation record](references/source.md) identifies the original guidance.
