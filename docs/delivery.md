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
