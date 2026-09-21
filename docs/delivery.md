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

The operator policy requires a local immutable `delivery_qualifications` record. That record pins `delivery:veto`, catalog `delivery-v1`, backend, model, decision policy, the entire automatic-delivery policy hash, probability threshold, and an artifact hash. The artifact is a bounded JSON held-out evaluation report with at least 50 cases, no loss in completion quality or critical-defect recall, and lower verified-completion time. Horde verifies the artifact hash and metrics before any automatic write. **There is no supported qualification enrollment command yet**, so automatic merge cannot be activated operationally by setting TOML alone. Phase 6 must add the evaluation, review, and promotion path that creates this record. Do not edit SQLite to bypass that path.

This policy covers the existing merge-triggered deployment contract. Independent release dispatch and rollback remain disabled until a specific provider contract can pin an immutable release artifact, reconcile a dispatch after a lost response, and identify a known-good rollback target.
