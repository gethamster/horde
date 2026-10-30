# Horde

Read and apply the [shared contributor guide](.agents/oss-delivery/AGENTS.md)
from [oss-delivery](https://github.com/gethamster/oss-delivery), pinned at
`1fa9eb4630de5885ae521d6af3b080245a3e9965` in [.agents/oss-delivery/lock.json](.agents/oss-delivery/lock.json).
The complete pack is available locally in `.agents/skills/`; `.claude/skills/`
contains regular-file entrypoints to the same instructions.

Always apply [verified-delivery](.agents/skills/verified-delivery/SKILL.md) and
[workspace-cleanup](.agents/skills/workspace-cleanup/SKILL.md). Solve the requested
problem, use proportionate proof, run relevant existing staging smoke/evals,
and ship once the outcome and actual repository gates pass. Reuse valid evidence.
Temporary repro tests may be removed when they add no continuing value. Do not
add coverage quotas, mandatory test-first work, independent review, full-suite
reruns, or another approval round by default. Carry existing user authorization
through delivery; ask only for a genuinely missing decision or permission.

These notes and nested contributor files add repository facts and affected
commands, not extra process gates. Product authorization and data boundaries
remain part of correct behavior. Runtime-agent instructions describe agents
running inside the product and do not override this contributor workflow.
Update shared guidance in oss-delivery, then sync the pinned copy; do not fork
its managed files locally.

## Repository scope

Horde is a Rust daemon with SQLite, CLI, and MCP interfaces. Read the relevant
[architecture](docs/architecture.md) before changing persistence or recovery.
Keep worker conversation separate from authoritative workflow/ownership state.
Preserve claims, durable operation identities, and correct provider usage.
Choosing a contributor model does not authorize substituting the customer's
configured model or provider in the product.

Use temporary repositories and existing fake endpoints for checks that would
otherwise mutate external systems. Do not add a mock architecture by default.
Live provider scripts may consume account/API capacity; run them only within
the requested scope and authorization. Keep credentials, runtime databases, and
worker transcripts out of Git.

## Relevant checks and delivery

Use focused Rust checks while fixing a bounded issue. [CI](.github/workflows/ci.yml)
and [Rust checks](.github/workflows/rust-checks.yml) define existing verification;
do not repeat the full matrix locally or add an independent review by default.
Documentation-only changes need content and link checks. Follow
[CONTRIBUTING.md](CONTRIBUTING.md) for affected generated catalogs and release
commands. Guidance-only changes do not require a binary release.

Use [people-first PR writing](.agents/skills/people-first-pr-descriptions/SKILL.md)
and [writer-responsible-prose](.agents/skills/writer-responsible-prose/SKILL.md)
for concise, factual descriptions. Neither adds validation stages. Keep skill
entrypoints as regular files because remote snapshots reject symlinks.
The `skills/` directory ships Horde workflows for runtime users; it does not
require contributors to execute those workflows for every repository change.
