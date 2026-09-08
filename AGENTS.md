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
