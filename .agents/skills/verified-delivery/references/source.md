# Adaptation record

Adapted from the user-supplied `always-run/SKILL.md`, selected for reuse on
October 29, 2026. Source SHA-256:

`c9d5a5178dc66aa818c67745b9745110404afd223787086dae3e76f968fd845f`

The source is preserved in the supplied download. This adaptation retains its
focus on successful operations, bounded recovery, meaningful verification,
evidence reuse, and completing authorized delivery. It replaces Tuarā-specific
market and release details with contributor guidance for the owning repository.
The original `ci-speed` dependency is not required by this self-contained skill.

`metadata.always-run: "true"` is an OSS Delivery convention for applicable
contributor changes, not a portable guarantee that every host preloads the skill.
The shared `AGENTS.md` supplies the explicit usage rule. In Codex metadata,
`policy.allow_implicit_invocation: true` permits automatic selection; it does not
guarantee invocation. Consuming repositories must make the skill and its
references available and carry the usage rule in their own instructions.

## Selected restraint guidance

On October 29, 2026, the user selected the missing principles from
`restraint/SKILL.md` for inclusion here rather than as a separate skill.
Source SHA-256:

`d91ee519bd7f384164bfd90e9a20aa8e7cc3b91666e30c109138f1698837c3b3`

The additions clarify ownership of build, release, migration, diagnostic, and
configuration entrypoints, and removal of tests tied only to retired
implementation details. Existing rules already cover simpler mechanisms,
evidence reuse, and proportionate verification. The project-specific mechanism
table and blanket ban on migration-check jobs were not adopted; required
repository checks and the owning migration workflow still apply.

## Selected incident-evidence guidance

On October 29, 2026, the user selected the evidence map from
`run-to-ground/SKILL.md` for inclusion as a resource in this skill.
Source SHA-256:

`ae411834f4fcf7674797c7b9ba7a8526a42573d80b6dc90840a80c90187ad602`

The resource maps operation, runtime, source, artifact, and recovery evidence to
their owners. Existing verification and recovery rules remain authoritative.
Tuarā-specific scripts, cloud services, and the exclusive Git-host rule were not
adopted; diagnostic commands belong in each consuming repository’s runbook.

## Solve-and-ship rebalance

The user requested a shorter always-run path focused on making the fix,
staging smoke/evals, a concrete pass/fail decision, and delivery without repeated
engineering debate. Temporary reproduction tests need not become permanent.
The detailed recovery and incident-evidence resources moved to the task-selected
`incident-recovery` skill. They are no longer loaded as a default repair procedure.
