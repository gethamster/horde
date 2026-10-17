## gstack

Use the `/browse` skill from gstack for all web browsing. Never use
`mcp__claude-in-chrome__*` tools directly.

## Project

This is a Rust daemon with embedded SQLite, CLI and MCP interfaces. Read
`docs/architecture.md` before changing persistence or recovery. Keep worker
conversation separate from authoritative workflow and ownership state.

For Rust changes, use `cargo fmt --all --check`,
`cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`
for verification. Choose checks by the changed behavior; documentation-only
edits need content and link checks, not Rust builds. Apply this scope when
following the PR-writing skills too. Tests must use temporary repositories
and mock external writes by default. Live provider scripts are opt-in and may
consume subscription/API capacity. Never commit credentials, runtime databases,
or worker transcripts.

## Deliver without process churn

- Carry the user's authorization and constraints through the whole task. Do
  routine implementation, checks, publication, and authorized release without
  asking again. Ask only for an unresolved decision the user must make; first
  finish the independent work and present the concrete decision and evidence.
- Give each distinct root cause one owner through verified delivery. Check for
  existing investigations and patches before assigning more agents. Parallelize
  independent fixes and review alongside builds; avoid overlapping diagnosis,
  chains of coordinating agents, and completed patches waiting for a handoff.
- Use one independent review. Reuse its evidence for unchanged patches and
  mechanical rebases; review substantive changes. A reviewer blocks on a concrete
  defect or violated requirement with evidence, not speculative reassurance.
- Run checks that establish the changed behavior once. Repeat only for changed
  behavior, a failure, or unresolved evidence. Add no approval, cooldown, test,
  or release requirement merely to make the agent feel more certain. Use the
  existing build and release mechanisms rather than starting duplicate runs.
- Reuse traces, reproduction results, and review evidence. Record the current
  conclusion and next action so another agent can continue without rediscovery.
  Repeat an investigation only when new evidence can change the decision.
- Fix a demonstrated queue or publication bottleneck before feeding it more
  agents. Preserve necessary authorization, financial correctness, ownership,
  and customer restrictions at their authoritative boundary; remove redundant
  checks and keep unrelated provider failures scoped to their own evidence.
- Report merged, released, installed, and positively verified work separately.
  Say "not shipped", "unverified", or "unknown" when applicable, and name the
  actual blocker and next action. Count distinct verified fixes separately from
  stale closures, duplicates, PRs, busy agents, tokens, and checks. Label daily
  counts with their time window and source; activity is not a delivery outcome.

Choosing a model or provider for development agents authorizes no substitution
of the models or providers in the system being built, operated, or tested.
Preserve the customer's requested model and restrictions. Success with one
model or provider is not evidence that another works.

## Writing

Use [people-first-pr-descriptions](.agents/skills/people-first-pr-descriptions/SKILL.md)
for every pull-request title and body (Studio’s people-first structure: What
changed → Why it matters → How it works → User impact → Scope → Validation →
optional Technical details). Use
[writer-responsible-prose](.agents/skills/writer-responsible-prose/SKILL.md)
when drafting or revising that prose so it stays human. Read its instructions
and the references relevant to the edit. Claude uses a regular-file entrypoint in
`.claude/skills/writer-responsible-prose` that points to the shared skill. Keep
repository skill entrypoints free of symlinks because remote snapshots reject them.

Include a Mermaid diagram of the relevant flow when it clarifies the change, and
validate it before publishing. State only checks that actually ran and any
remaining limits.
