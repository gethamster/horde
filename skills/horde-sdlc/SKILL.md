---
name: horde-sdlc
description: Coordinate an AI-native software development lifecycle with Horde using intent, specifications, implementation plans, verification evidence, and review handoffs. Use when adopting Anthropic's SDLC playbook, continuing work from intent.md or spec.md, or turning an incident into a bounded Horde workflow.
---

# Horde SDLC

Use Horde's existing tasks, pinned context, skills, and verification commands to
carry work between SDLC stages. This skill adapts
[Anthropic's AI-native SDLC playbook](https://academy.claude.com/courses/ai-native-sdlc-playbook)
for Claude Code, Codex, and native workers. It supplies workflow guidance; it adds
no runtime operation, automatic Markdown parser, or approval enforcement.

## Establish the handoff

Start at the stage the user requested. Read the supplied artifacts and existing
project conventions before creating files. An accepted specification can proceed
to planning; a review request does not require rebuilding the whole lifecycle.
For a small change, keep the record proportionate to the work.

Identify the intended outcome, current stage, authoritative artifact location,
source revision, acceptance criteria, and authorized next action. Preserve the
user's artifact structure and system of record. Jira or another requirements tool
may own the record; repository Markdown can be a working copy with a source link.
Use available connectors only within their authorized scope. Report inaccessible
sources rather than inventing their contents or replacing their authority.

Read [artifact handoffs](references/artifacts.md) when importing or writing
intent, specifications, plans, or evidence. Capture the actual source content and
revision needed by workers. A mutable URL alone is insufficient execution context.

## Choose the work

| Requested stage | Work to perform | Handoff |
| --- | --- | --- |
| Plan | Clarify the problem, affected users, constraints, and success criteria | An intent artifact for its owner's decision |
| Design | Derive requirements and design from accepted intent; surface policy conflicts | A specification with testable criteria and unresolved decisions |
| Build | Plan against the accepted specification, then assign bounded implementation work | A committed implementation with its plan and validation evidence |
| Test | Verify combined behavior and evaluate agent configuration when relevant | Results tied to the tested commit and configuration |
| Deploy | Review against the specification, plan, and applicable review policy | A reviewable branch or authorized PR; release through existing controls |
| Maintain | Diagnose a supplied incident or monitoring signal | Proposed new intent with evidence and a regression case |

Apply relevant repository guidance, including `CLAUDE.md`, `AGENTS.md`,
`REVIEW.md`, and organizational skills when present. Preserve their authority and
scope; do not copy Claude-specific settings into another harness as if they were
portable. Pin selected skills through Horde's existing catalog. When operating
inside a task, use `list_skills` and `read_skill` to read the pinned versions.

## Execute through existing Horde tools

Read [execution and review](references/execution.md) before submitting work or
changing a running workflow. It covers phase boundaries, context changes,
verification, and delivery using the current CLI/MCP surface.

Use the available `horde-planning`, `horde-delegation`, and `horde-templates`
guidance when choosing workers or authoring a project template. These skills are
optional helpers; discover them before reading them. Preserve the user's runtime
and model choices, and validate the actual template before submission.

Keep a required approval outside the automatically runnable portion of the
workflow. The caller can finish a preparation task and submit the next phase once
the existing approval requirement is satisfied. Honor authorization already given
for the relevant artifacts and action; do not ask again just because a stage has
a new name. Record the approver or authoritative decision source and the revision
it covers. If the relevant content changes, reassess that decision's scope.

Human-only questions are useful for an unresolved decision during work. Their
external caller attestation is not authenticated organizational approval. Skills,
model acceptance, and notebook claims do not replace protected branches, managed
harness controls, or environment approvals where those are required.

## Return the result

Report the source revisions, completed stage, result commit, actual checks,
remaining findings, and next action. Distinguish an agent's success report from
command evidence and external approval. A local branch is a valid result when
publication was not authorized. Preserve uncertainty after interrupted work and
inspect recorded effects before retrying.

For maintenance, use a supplied signal or an existing authorized monitoring
integration. Deduplicate by its incident/event identity before submitting work.
Horde's delivery health check does not continuously monitor production. Diagnosis
does not authorize a fix, deployment, rollback, or recurring monitor; use the
existing policy and user authorization for each action. Carry an accepted incident
into the normal workflow and retain its regression check with the resulting fix.
