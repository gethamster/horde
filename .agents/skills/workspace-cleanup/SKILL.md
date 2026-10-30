---
name: workspace-cleanup
description: >-
  Always check task-owned resources at contributor handoff or completion, retire
  them when no longer needed, and respond to workspace resource pressure. Safely
  clean worktrees, temporary processes, build outputs, and bounded caches while
  preserving active work, runtime state, artifacts, and required evidence.
metadata:
  always-run: "true"
  audience: "contributors"
---

# Workspace Cleanup

Complete the task’s resource lifecycle as part of delivery. Retire task-owned
resources as soon as their work is delivered or integrated into the retained
candidate and no remaining work needs them. Check again before handoff or
completion and when disk pressure threatens active work.

Always-run means always assess the task’s owned resources, not always delete
something. If the task created or borrowed no disposable resources, finish with
no cleanup action. Do not turn a scoped cleanup into a machine-wide sweep,
background monitor, or deletion of unrelated work.

## Establish scope and ownership

Start from resources recorded or created during the task. Identify each resource’s
actual owner, purpose, active consumers, and remaining use. Resolve paths and
worktree identity before acting; a name, age, directory suffix, or location under
a temporary directory is not proof that something is disposable.

Keep the retained working checkout and any active, primary, pinned, shared, or
handoff-required resources. Do not remove the current working directory. Check
for running processes and concurrent work again before the destructive step.
If ownership or continued use is uncertain, retain that item and state the
specific missing evidence while cleaning other verified disposable resources.

## Preserve before removal

Use [Preservation and evidence](references/preservation-and-evidence.md) to
classify source, uncommitted and untracked work, ignored artifacts, runtime state,
and required evidence. Preserve needed material in its owning repository or
another authorized durable location outside the resource being removed. Verify
the preserved copy before deletion; planning to preserve it is insufficient.

Never treat credentials, application data, backups, state files, upstream ledgers,
or required release and investigation evidence as routine cleanup targets, even
when they are ignored by Git or stored under a cache or temporary path.

## Retire the resource through its owner

- For task-owned worktrees, follow [Worktree retirement](references/worktrees.md).
  Check active use, all working changes, unique commits, and required ignored
  files. Use managed archival for attached worktrees and supported Git removal
  for other worktrees after preservation.
- For temporary processes, build outputs, and caches, follow
  [Processes and caches](references/processes-and-caches.md). Stop only identified
  task-owned processes through their lifecycle controls. Remove only derived
  outputs that no active or planned validation needs.
- Reuse compatible build caches and maintain an explicit size bound. Reclaim
  inactive disposable data within that bound before creating another full cache
  per worker. Do not sacrifice protected state to meet a disk target.

Routine retirement of verified task-owned disposable resources is part of
completing authorized work; do not ask again solely because it is cleanup. A
request to finish a task does not authorize deletion of unrelated repositories,
shared services, user data, or protected records. Obtain the missing scope or
permission only for the affected item. Never use force flags to bypass missing
ownership, activity, or preservation proof.

## Verify and report

Confirm the intended resource was retired, the preserved material is accessible,
and retained work remains usable. Reconcile stale registrations through the
owning tool after checking that they are actually stale. Reinspect uncertain
removal outcomes before retrying.

When reclaiming disk, measure actual free space before and after; logical file
sizes or shared layers do not prove physical bytes reclaimed. Report removed
resources, preserved work, intentional retention, and any blocked cleanup. A
handoff must identify retained resources and their next owner or remaining use.
Do not call cleanup complete while a known disposable task-owned resource remains
unhandled; a justified active or protected retention is a different outcome.
