---
name: technical-documentation
description: >-
  Use when documenting or changing service contracts, APIs, authentication,
  forwarding, storage ownership, deployment topology, or operational runbooks,
  and when auditing technical documentation against source and runtime evidence.
  Keep public and internal references accurate using the repository's existing tools.
metadata:
  audience: "contributors"
---

# Technical Documentation

Keep technical documentation aligned with the behavior and ownership it describes.
Follow [verified-delivery](../verified-delivery/SKILL.md) for scope, authority,
verification, and delivery. Use the repository’s existing documentation structure,
contract definitions, and generators; this skill does not require a new registry,
OpenAPI specification, diagram system, or publishing pipeline.

## Establish the evidence and audience

Read the relevant implementation, caller, serialization types, configuration,
and existing documentation as needed to support the changed claim. A small text
correction needs its immediate context, not a new system-wide trace. For an
operational claim, inspect the
actual runtime evidence when it is available within the task’s authority. Source
code or desired configuration alone does not prove a feature is deployed.

Identify the owner of the behavior and the audience for each document. Separate
public developer contracts, application-specific interfaces, internal service
contracts, and operational procedures. Use exact names, versions, methods, and
paths supported by evidence. Report uncertainty instead of filling gaps with
plausible contracts or deployment claims.

## Update the owning documents

Use [Service contracts](references/service-contracts.md) for routes, schemas,
authentication, forwarding, streaming, errors, retries, and compatibility.
Update the canonical contract and affected consumer documentation together.
Publish only interfaces intended for that audience; keep private configuration,
credentials, and internal-only details in their authorized locations.

Use [Architecture and operations](references/architecture-and-operations.md)
when runtime responsibilities, data ownership, deployment boundaries, or runbook
steps change. State which component owns an operation and which only forwards
it. Distinguish contributor tooling from agents and services running in the
product. Preserve meaningful decision history while updating current-state docs.

Prefer one canonical explanation with links to deeper material. Correct stale
competing descriptions within the changed scope; do not start an unrelated
rewrite or create a glossary or documentation hierarchy merely to satisfy this
skill. When code disagrees with intended policy, record the discrepancy rather
than describing a defect as approved behavior.

## Regenerate and verify

Follow [Documentation verification](references/documentation-verification.md)
for affected links, examples, generated references, diagrams, and publication.
Edit the owning source for generated documents and use the established generator;
do not patch generated output by hand or require a generator the repository lacks.

Verify changed behavior through the owning repository’s applicable checks. A
prose edit needs relevant content and link checks; a generator change needs its
relevant executable checks, reusing existing tests where suitable; a runtime
contract change needs relevant behavior evidence. Do not add
unrelated application builds or new approval gates to documentation work.

Report the documents and generated outputs changed, checks actually performed,
and any important difference between described, configured, tested, and deployed
behavior. A saved local document is not automatically published documentation.

The [adaptation record](references/source.md) identifies the original guidance.
