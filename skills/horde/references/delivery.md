# GitHub delivery

Delivery is off by default. Nothing is pushed, no PR is opened, and no deployment
is observed unless you explicitly enable it.

```toml
[delivery]
enabled = true
repository = "owner/repo"
base = "main"
merge = true
deploy_workflow = "deploy.yml"
health_url = "https://example.com/health"
```

Authenticate `gh` through its own credential store first. The configured
`repository` must match the checkout's `origin`, or delivery fails.

## What a delivery step does

1. Pushes the `horde/TASK_ID` branch.
2. Finds or creates its pull request against `base`.
3. Watches the PR checks.
4. Compares the verified head before merging, then merges.
5. Observes the configured **push-triggered** deployment workflow for the merge
   commit.
6. Checks `health_url`.

Set `merge = false` to stop at a checked PR.

Horde never force-pushes, never bypasses branch protection, and never dispatches a
duplicate deployment job.

## Idempotency and failure

External operation intents and the identities they return are durable. A PR or
merge retry queries GitHub state before acting: after a lost response it finds the
existing PR rather than opening a second one.

These stay failures, with evidence, and are not retried into success:

- An unexpected head (something changed after verification).
- A closed PR.
- Merge conflicts.
- Failed checks.
- A failed health check.

A transient external failure can be retried by resuming the delivery step. A
failure that needs code changes belongs in an explicit repair branch in the
template; Horde does not invent an unbounded repair loop.

## Templates

The built-in `github-actions` template composes Next.js verification with a
delivery step. For any other project, include your own local template and follow
it with a `kind = "delivery"` step. See the `horde-templates` skill.

## Before you enable it

Delivery writes to a real repository and can merge to a real default branch. Get
explicit confirmation from the user before enabling `merge = true` on a repository
they care about, and prefer `merge = false` on the first run so a human reviews
the PR Horde produced.

Live GitHub delivery is exercised in this release against a deterministic CLI
fixture and a local Git remote, not against a live production deployment. Treat
the first real run as something to watch.
