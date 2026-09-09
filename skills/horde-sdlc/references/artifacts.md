# Artifact handoffs

Use the project's current artifact home and format. Where none exists, propose a
change-specific directory such as `intent/<change-id>/` containing `intent.md`,
`spec.md`, and `plan.md`. These paths are conventions, not Horde-reserved names.
Create only the artifacts needed for the requested work. Do not overwrite an
unrelated change's files or make a parallel source of truth without agreement.

## Content to retain

| Artifact | Information needed by the next stage |
| --- | --- |
| Intent | Problem, affected users and systems, desired outcome, constraints, non-goals, open questions, and owner |
| Specification | Source intent revision, observable requirements, acceptance criteria, design choices, policy concerns, and resolved or outstanding decisions |
| Implementation plan | Source specification revision, file scopes, dependency order, risks, verification commands, and completion criteria |
| Review evidence | Reviewed commit, source artifact revisions, policy/skill versions, criterion results, findings, and approval references |
| Incident-derived intent | Event identity, observation time, affected release/system, diagnostic evidence, proposed outcome, and triage decision |

Keep stable criterion identifiers when the source supplies them. If it does not,
assign local identifiers without changing the requirements. Separate unanswered
questions from decisions. A draft's `approved` label does not prove that its owner
approved it; retain the actual decision reference.

If an accepted artifact conflicts with its authoritative source, compare both
revisions with the recorded authorization before submitting work. Do not silently
choose whichever is newer. Resolve only the missing decision; an existing
approval that clearly covers the current source does not need to be repeated.

For repository sources, record path and commit SHA and capture the bytes used.
For external records, retain the record ID or URL, revision when available, and
retrieval time. If no revision is exposed, hash the retrieved content and identify
that limitation. State whether the external record or its repository copy is
authoritative. Dirty local files need an explicit content snapshot; do not present
them as part of a commit that does not contain them.

## Evidence manifest

Use an existing report format if the project has one. Otherwise write a compact
Markdown table or JSON artifact linking the following information. This is a
report convention, not a new Horde schema or a machine-enforced certificate.

- Source artifacts: identity, revision or content hash, and authority.
- Execution: task/attempt IDs, template revision, selected skill hashes, and
  recorded executor/model identities where available.
- Result: the exact integrated commit and any PR or deployment identity.
- Criteria: criterion ID, check or review method, observed result, tested commit,
  and evidence location. Use pass, fail, not run, or inconclusive accurately.
- Decisions: approval reference, scope and artifact revision, reviewer findings,
  unresolved risks, and the next authorized action.

Keep application-test results separate from agent-eval results. Evals should name
the cases, expected outcomes, configuration under evaluation, baseline if used,
and measured results. A model/prompt/skill change may require running an existing
eval suite even when application tests pass. Do not fabricate a baseline or claim
an eval ran because the skill was loaded. Live model evals require permission to
consume the relevant capacity.

Artifacts belong in the task's existing artifact store or the project's agreed
output location. `put_artifact` accepts `name`, `content`, and an `inputs` object;
use input revisions/hashes to describe what produced it. Worker writes remain
unverified. Do not set `verified` to manufacture evidence, and do not confuse a
content integrity hash with proof that the artifact's claims are correct.

Avoid a circular commit claim when committing the evidence report itself. Record
the implementation commit that was tested, then distinguish any later report-only
commit. If code or relevant configuration changes afterward, rerun affected checks
and obtain a fresh final review for the resulting commit.

## Playbook sources

This adaptation uses the published lessons read on 2026-09-09. Team artifact
formats and approval policies take precedence over these examples.

- [Capture as intent.md](https://academy.claude.com/courses/ai-native-sdlc-playbook/capture-intent)
- [Requirements and design](https://academy.claude.com/courses/ai-native-sdlc-playbook/requirements-and-design)
- [Plan mode and artifact ownership](https://academy.claude.com/courses/ai-native-sdlc-playbook/plan-mode)
- [Continuous evals in CI](https://academy.claude.com/courses/ai-native-sdlc-playbook/continuous-evals-in-ci)
- [Review policy](https://academy.claude.com/courses/ai-native-sdlc-playbook/ai-in-the-pr-review-loop)
- [Maintenance feedback](https://academy.claude.com/courses/ai-native-sdlc-playbook/closing-the-loop-on-metrics)
