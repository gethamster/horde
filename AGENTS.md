## gstack

Use the `/browse` skill from gstack for all web browsing. Never use
`mcp__claude-in-chrome__*` tools directly.

## Project

This is a Rust daemon with embedded SQLite, CLI and MCP interfaces. Read
`docs/architecture.md` before changing persistence or recovery. Keep worker
conversation separate from authoritative workflow and ownership state.

Use `cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`,
and `cargo test --locked` for verification. Tests must use temporary repositories
and mock external writes by default. Live provider scripts are opt-in and may
consume subscription/API capacity. Never commit credentials, runtime databases,
or worker transcripts.

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
