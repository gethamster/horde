---
name: local-verification
description: >-
  Use when changing or diagnosing a local service installation, startup or restart
  behavior, configuration, identity, seed data, dependency connections, or user
  journeys across running services. Verify the intended revision and actual
  operation while preserving shared environments and state.
metadata:
  audience: "contributors"
---

# Local Verification

Verify the affected user operation against the intended running services.
Follow [verified-delivery](../verified-delivery/SKILL.md) for scope, authority,
recovery, and delivery evidence. A running process or healthy endpoint does not
establish that the requested workflow works.

Use the repository’s actual installation profile and lifecycle tools. Services
may run as native processes, under a service manager, in a sandbox, or through
another supported runtime. Do not assume a container runtime or replace the
owning installation mechanism with an improvised launcher.

## Identify the environment

Read the current runbook and service configuration. Establish the environment
owner, installed profile, service revisions, state locations, endpoints, and
relevant dependencies for the services being touched. Reuse a still-current
environment record; do not inventory the whole installation for each fix. Inspect
the existing running instance before starting another one.

Use [Service lifecycle and state](references/service-lifecycle.md) when startup,
configuration, credentials, restarts, or persistent data are involved. Coordinate
mutations with the owner of a shared environment; other consumers may rely on it.
Stay within the authorized local environment even when its configuration names
remote dependencies.

## Verify the affected journey

Choose the smallest existing smoke or end-to-end check that exercises the change
through the owning interfaces. Use [User journeys](references/user-journeys.md)
for identity, durable state, asynchronous work, and recovery checks. Preserve
the intended account scope, input, model or provider, and expected result.

Check which source or artifact is actually serving before diagnosing a failed
repair. Rebuilding a binary or merging a change does not update a running service.
Use its supported restart or reload mechanism when needed and authorized, then
verify its revision and the affected operation.

Distinguish process availability, dependency readiness, authenticated access,
and a successful user operation. Report which layers were exercised. Reuse
valid existing evidence; do not run an unrelated full suite for every local edit.

## Preserve existing state

Test fresh-install behavior with isolated disposable state and distinct runtime
identities. Test restart and upgrade behavior against preserved state when that
is the requested claim. Never erase a shared installation to manufacture a
successful fresh start.

Before cleaning up a failed attempt, retain relevant operation identities,
service versions, and diagnostic evidence without exposing credentials or private
content. Investigate failures that depend on prior activity without silently
substituting a new environment. Clean up only owned temporary processes and data
through the established lifecycle mechanism.

## Report the result

Record the tested environment, relevant service and artifact identities, journey
performed, observed result, and limits. A synthetic sign-in does not qualify real
sign-in; a successful local run does not establish production readiness. Identify
remote dependencies exercised and any effects that remain uncertain.

The [adaptation record](references/source.md) identifies the original guidance.
