# Worktree retirement

Retire a task-owned worktree after its changes are delivered or integrated and
its remaining consumers no longer need it. Keep unrelated and unfinished work.
Use the repository or application that owns the worktree; do not manually delete
Git administrative directories.

## Inspect the actual checkout

Read the worktree list and identity from its owning tool or repository. Determine
whether it is a primary checkout, an attached managed worktree, a locked checkout,
or an ordinary linked worktree. Verify the canonical path and its relationship
to the repository before choosing a removal method.

Useful read-only Git checks, scoped to the known repository or worktree, include:

```sh
git -C <repository> worktree list --porcelain
git -C <worktree> status --short --untracked-files=all
git -C <worktree> diff --stat
git -C <worktree> diff --cached --stat
git -C <worktree> log --oneline <retained-ref>..HEAD
```

Choose the real retained ref from the task; do not assume `main` contains its
work. Inspect required ignored files separately. Stop a stalled read rather than
allowing it to block cleanup indefinitely; retain the affected item until the
missing inspection can be completed.

Check active workers, processes, test runs, and service use. A clean Git status
only describes tracked and untracked working state, not active consumers or
needed ignored artifacts. Recheck activity immediately before removal and
coordinate with the owner if another task may start using the checkout.

## Prove the work is retained

Check staged and unstaged changes, untracked files, unique commits, and required
ignored artifacts. Use [Preservation and evidence](preservation-and-evidence.md)
for their destinations and access controls.

An ancestry check alone is insufficient after a squash merge or cherry-pick.
Compare the intended changes with the retained candidate or preserve the unique
commits explicitly. Do not delete a branch or tag that is the only remaining
reference to work that must survive.

## Use supported retirement

For an application-managed attached worktree, use its archive operation after
preserving anything the archive excludes. Confirm the returned archive or
recovery reference before treating preservation as complete. If the tool refuses
a primary, shared, active, or otherwise protected checkout, respect that refusal;
do not bypass it with direct filesystem deletion.

Move cleanup execution to a retained checkout, or hand retirement to the
appropriate owner, before removing the completed task’s working directory.
Keep that handoff explicit if the current task cannot safely leave the checkout.

For an ordinary Git worktree, use `git worktree remove <worktree>` only after the
checks above establish that removal is safe. Preserve needed dirty work first.
A failure or lock is evidence to investigate, not a reason to add force flags.
Do not remove the current working directory or the retained active candidate.

After retirement, verify the resource is absent or archived as intended, the
retained repository is usable, and worktree registration agrees with reality.
Remove task branches only after their needed content and history are preserved.
If stale administrative entries remain, inspect the owning tool’s dry-run or
listing before a scoped prune; do not prune unrelated or temporarily inaccessible
checkouts merely because their paths are unavailable now.
