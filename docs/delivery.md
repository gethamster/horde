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
