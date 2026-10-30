# Processes, build outputs, and caches

Clean only identified task-owned resources whose remaining purpose is complete.
A process name or common directory suffix is not enough to establish ownership.

## Temporary processes

Use the task’s process handle, lifecycle manager, or service record to identify
the exact process, its working directory, and its consumers. Process identifiers
can be reused, so recheck identity before signaling. Do not use broad name-pattern
kills to stop a single task’s helper.

Stop the process normally through its owning tool so children and temporary state
can settle. Confirm the process and relevant listeners have stopped. Preserve
required diagnostic output before removing its files. If stopping is uncertain,
inspect the original handle before retrying rather than launching or killing
another instance.

A service still used by an active task or shared environment is not disposable.
Coordinate with its owner; retain it for the continuing consumer and record that
handoff. This applies even when the current task originally started it.

## Build outputs

Identify the exact output paths from the task’s build commands or repository
configuration. Check active compilers, tests, package operations, and planned
validation before removing them. Keep the tested artifact and evidence needed
for integration, release, rollback, or handoff in their established durable home.

An output directory may contain runtime state, signed artifacts, or valuable
ignored files. Inspect its role and contents before treating it as reproducible.
Resolve symlinks and mount boundaries; do not follow an apparent output path into
another owner’s data. Avoid broad cleanup across sibling repositories, the whole
temporary directory, or every directory named `target`, `dist`, or `.cache`.

## Reusable caches

Prefer a compatible existing cache over one full cache per worker. Establish an
explicit size bound from the project policy or a measured resource budget, and
record the scope to which it applies. Account for repository, toolchain, target,
profile, instrumentation, permissions, and concurrent writers before sharing.
A cache must not mix incompatible outputs or cross a protected trust boundary.

Use the tool’s scoped cache cleanup or prune controls when available. Select only
inactive disposable entries and preview the selection when supported. Keep
caches needed by active or planned work within their recorded bound; remove
redundant caches when their consumers finish. Do not convert a resource shortage
into permission to delete backups, state, or required evidence.

Measure actual free disk space before and after reclamation. Logical artifact
sizes can overstate reclaimed space because of shared layers, clones, and open
file handles. Report measured recovery or explain why it could not be measured.

## Resource pressure

Inspect the task’s largest known disposable outputs first. Preserve work and
verify inactivity before reclaiming them, just as at ordinary completion. Do not
invent a universal free-space threshold or add a cleanup daemon. When safe scoped
reclamation is insufficient, report the remaining resource limit and the specific
retained resources rather than deleting unrelated data to force progress.
