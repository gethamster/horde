# GitHub delivery

`github-actions` composes Next.js verification with GitHub delivery. Enable delivery explicitly:

```toml
[delivery]
enabled = true
repository = "owner/repo"
base = "main"
merge = true
deploy_workflow = "deploy.yml"
health_url = "https://example.com/health"
```

Authenticate `gh` using its credential store first. The configured repository must match the checkout's `origin`. `base` also chooses where the task worktree branches from: Horde fetches `origin/<base>` before allocating it, so the PR compares against the current base rather than a stale local checkout. The runtime pushes the result branch, finds or creates its PR, watches checks, compares the verified head before merging, observes the configured **push-triggered** deployment for the merge commit, and checks health. Set `merge = false` to stop at a checked PR. It never force-pushes, bypasses branch protection, or dispatches duplicate deployment jobs.

External operation intents and returned identities are durable. PR and merge retries inspect GitHub state first. An unexpected head, a closed PR, conflicts, or failed health checks remain failures with evidence.

`horde summary TASK_ID`, the last line of `horde watch`, and `task.finished` notifications report the delivery outcome. With `merge = false` a finished delivery step reports `pr_ready` with the PR URL in `pr_url`, so a script knows where to send a reviewer. A template without a delivery step reports `skipped` with the reason `template has no delivery step`, and a delivery step under `enabled = false` reports `skipped` with `delivery disabled in settings ([delivery] enabled = false)`. Both copy the reason into a top-level `delivery_skipped` field, so a succeeded task with no PR is explained rather than silent. See [progress and notifications](progress.md) for the full summary object.

## Guarded automatic delivery

An operator-owned `[automatic_delivery]` policy is separate from a repository's `[delivery]` settings and is disabled by default. Repository files cannot set it. When enabled for a task with `delivery.merge = true`, the policy narrows the existing push-triggered path to a named repository, base, deployment environment, workflow, check set, and changed-path prefixes. The trusted GitHub CLI path comes from the operator policy; `delivery.program` cannot replace it. GitHub and Git commands in this path receive no application-bundle secrets.

The operator's `~/.config/horde/config.toml` can describe the scope, but the configuration alone cannot authorize a merge:

```toml
[automatic_delivery]
enabled = false
gh_program = "/absolute/path/to/gh"
repositories = ["owner/repo"]
bases = ["main"]
environments = ["staging"]
deploy_workflows = ["deploy.yml"]
allowed_paths = ["src/ui"]
required_checks = ["test"]
minimum_approvals = 1
require_independent_review = true
version_url = "https://example.com/version"
smoke_url = "https://example.com/smoke"
qualification_id = "held-out-evaluation-id"
minimum_routine_probability = 0.8
minimum_review_confidence = 0.8
```

The project's `[delivery]` section must still name the same repository, base, `environment`, and `deploy_workflow`. Before creating the PR, Horde captures a complete Jev review checkpoint and a separate generative reviewer result. The reviewer step must declare and return `reviewed_head`, `reviewed_revision`, `verdict`, `risk`, `coverage_complete`, and `findings`; only an independent, passing, routine result for the final integrated commit qualifies. The latest reviewer result wins, including a failed or adverse result. The Jev checkpoint must have full diff coverage, no flagged signals, and confidence above the operator floor. Automatic delivery also requires an unmodified text diff of at most eight files, with no rename or deletion; suspicious security, migration, and public API changes hold for manual review.

Immediately before merge, Horde checks the exact PR head and base, successful named check-run IDs, current review IDs and approvals, clean integrated workspace, task context, and current operator policy. A qualified Jev delivery decision may escalate the change; it cannot waive a failed deterministic gate. The authorization and merge intent are separate immutable records. The target branch must enforce strict up-to-date checks and the required approval count for administrators without bypass actors. `gh pr merge --match-head-commit` binds the head, while branch protection prevents a stale-base merge; Horde also verifies the squash commit's parent against the authorized base after merging. A changed base or unavailable protection evidence holds delivery.

The selected deployment run must be a unique `push` run for the squash commit on the expected base. Horde verifies its run ID and successful completion, then queries operator-owned version and application smoke endpoints. The version endpoint must return JSON with `commit` equal to the merge SHA and `environment` equal to the selected environment. The smoke endpoint must return the same identity plus `"ok": true`. HTTPS is required except for literal loopback HTTP used in local tests. The existing `health_url` remains an optional general health probe; it does not establish deployment identity.

The operator policy requires a local immutable `delivery_qualifications` record. That record pins `delivery:veto`, catalog `delivery-v1`, provider, model, decision policy, the entire automatic-delivery policy hash, probability threshold, and an artifact hash. The artifact also binds the provider, normalized endpoint, protocol, model, and credential-variable identity. It is a bounded JSON held-out evaluation report with at least 50 cases, no loss in completion quality or critical-defect recall, and lower verified-completion time. Horde verifies the artifact hash and metrics before any automatic write. An operator can enroll an independently reviewed retained report with `delivery_qualification_enroll`; see [upstream maintenance](upstream-maintenance.md). Configuration alone does not authorize a merge. Do not edit SQLite to create qualification records.

This policy covers the existing merge-triggered deployment contract and the explicit, default-off [Adam immutable CLI release contract](upstream-maintenance.md). Other independent release dispatch and rollback remain disabled.

## Producing genuine held-out evidence

`horde delivery-qualification` prepares a frozen corpus, collects existing Run
evidence through read-only daemon RPCs, validates it and computes a reviewable
report. It does not select cases, dispatch agents, invoke providers, authorize
spend, open a Store, mark worker artifacts verified or enroll the report.

```sh
horde delivery-qualification prepare --manifest /private/corpus.json --output /private/frozen.json
horde --data-dir /existing/daemon --project PROJECT delivery-qualification collect --frozen /private/frozen.json --output /private/evidence
horde delivery-qualification validate --frozen /private/frozen.json --evidence /private/evidence
horde delivery-qualification report --frozen /private/frozen.json --evidence /private/evidence --output /private/report.json
```

`validate` exits unsuccessfully when evidence is incomplete or the quality,
critical-defect recall or verified-completion comparison fails. `report` retains
the computed outcome for review, including failures. Neither command accepts
aggregate metrics supplied by the caller. Incomplete evidence produces blockers
without a fabricated `held_out` section. Files are private and retained by hash;
retrying collection reconciles the same immutable evidence. A changed corpus,
Run head or existing report holds the operation instead of overwriting it.

The manifest has `schema_version: 1`, `qualification_id`, `spend_reference`,
`oracle_id`, `oracle_spec_sha256`, the exact `decision` configuration, the final
`delivery_policy`, and at least 50 `cases`. The schema rejects unknown fields.
The decision configuration contains credential-variable references, never keys.
It must identify the actual Jev-compatible `systemone-v1` service; a successful
native Chat Completions probe does not qualify that separate endpoint. Tuara’s
established base is `https://tuara.com/router`, which appends `/v1/systemone`;
the retained September routing report used ticker `XXXXTSJV130XXX`. Historical
connectivity does not establish current held-out qualification.

Each case freezes a unique `id`, `repository`, `base_commit`, `objective`,
`inputs_sha256`, `verification_argv_sha256`,
`baseline_acceptance_question_sha256`, `critical`, `baseline_task`, and
`candidate_task`. SHA-256 values use lowercase hexadecimal. The verifier argv
hash covers its canonical JSON array. The acceptance-question hash covers the
exact operator-reviewed text, with `accept` or `escalate` answers. Input hashes identify independently
reviewed case material. At least one case must contain a real critical defect
so recall has a denominator. The corpus and timestamp are frozen together
before candidate execution, including the assigned Run IDs; existing candidate
attempts cannot be renamed into a fresh held-out sample. Same-second source
timestamps cannot establish that ordering and remain unqualified.

The baseline needs actual conventional/manual-acceptance Runs with the same
case inputs, verifier and independent oracle. Existing historical Runs are usable
only when those facts and their real completion events can be established.
The baseline must have automatic delivery disabled, the frozen acceptance
question on the actual Run, and a matching protected `question.answered` event
within its measured completion interval. The oracle records that question ID
and observed answer. Otherwise, genuine paired execution and baseline human participation are missing
inputs. A requested change is not evidence of what a human would have accepted,
how long that would have taken or which defects they would have found.

The owning operator must review and retain these task-linked artifacts through
the supported artifact interface. The collector requires exactly one verified
link of each applicable name; worker verification claims do not qualify:

| Artifact | Required content from actual evidence |
| --- | --- |
| `qualification-inputs` | `manifest_hash`, `case_id`, `repository`, `base_commit`, `inputs_sha256`, `objective_sha256` |
| `qualification-oracle` | `manifest_hash`, `case_id`, `task`, `head_sha`, `oracle_id`, `oracle_spec_sha256`, `reviewer_id`, `critical_defect_present`, `verified_completion`, `quality_passed`, `critical_defect_detected`; baseline also binds `manual_acceptance_question`, `manual_acceptance_answer` |
| `qualification-github-receipts` (candidate) | Actual `repository`, `base_oid`, `observed_at`, `pull_request`, `check_runs`, and `reviews` API receipts |
| `qualification-decision` (candidate) | Actual `request` and `response`, `purpose: delivery:veto`, `catalog: delivery-v1`, `backend_fingerprint`, `delivery_policy_hash`, `transport_sha256`, `observed_at` |
| `qualification-decision-transport` (candidate) | Reviewed redacted trace identity: `endpoint`, `http_status`, `request_sha256`, `response_sha256` |

Do not manufacture these artifacts from expected outcomes or a model's account
of its work. The oracle must report actual independent verification and defect
labels fixed before evaluation. Its reviewer must differ from every Run worker.
The candidate's real advisory request uses the production delivery-veto question
and the complete production preflight state reconstructed from retained Run
changes, the runtime review checkpoint and its original evidence artifact, actual
independent reviewer attempts, and GitHub receipts. Required checks, approvals,
path restrictions and review predicates reuse production validation. Missing
fields or facts that appeared after the advisory decision fail qualification.
The retained transport trace must
bind the normalized endpoint and canonical request/response hashes. Credential
values and private provider transcripts do not belong in committed fixtures.

Advisory evaluation does not require an automatic merge to run. Before the first
qualified installation, a separately authorized operator may retain actual
non-publishing Jev request/response traces on the candidate Run for review. This
driver only collects them; it does not issue that paid evaluation. A currently failing
Jev connection, missing account, absent spend approval or missing real trace is a
blocker, not permission to insert a decision or qualification row.

Quality and critical recall are computed from the independent oracle's observed
outcomes across the complete frozen corpus. Completion time comes from actual
Run creation and matching terminal events, with one-second source precision;
it includes queueing and the actual advisory evaluation. A provider response
recorded after the candidate finished cannot be omitted from its timing. Provider
request latency, estimated human delay, timeout constants and random values are
not substitutes. A candidate with no genuine latency improvement fails.

Activation still needs the frozen reviewed corpus, real baseline and candidate
Runs, current independently verified heads, independent oracle records, actual
Jev transport evidence, a matching approved worker-budget reference and the
operator's review of the final report hash. `delivery_qualification_enroll` remains
a separate operator-only step. A computed passing report does not grant shipping
authority or waive repository protections, acceptance, expiry or revocation.
