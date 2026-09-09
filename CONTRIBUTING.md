# Contributing

Use the pinned Rust toolchain and keep the lockfile committed. Run formatting,
Clippy with warnings denied, and the full test suite before proposing a change:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Add behavioral tests for changes to ownership, persistence, recovery, provider
contracts, and external writes. Use isolated temporary repositories and fake
provider endpoints by default. Live scripts are opt-in and may consume account
capacity; never require personal credentials in CI. Do not commit keys, runtime
databases, transcripts, or temporary worktrees.

Treat schema changes as migrations. Never silently downgrade an unknown database
version, release an uncertain worker's claims, retry an un-reconciled external
write, or turn missing provider usage into zero-cost usage. Match docs and examples
to the actual supported contract.

When a change affects configuration, tool arguments, or workflow behavior, update
the matching guide and the references under `skills/`. Keep the guide tables in
[README.md](README.md#documentation) and [docs/README.md](docs/README.md) aligned.
Regenerate the published tool catalog when schemas change:

```sh
UPDATE_WEBSITE_SPEC=1 cargo test --test website_spec
```

See [release CI](docs/installing.md#release-operator-setup) for platform checks,
build ordering, and cache policy.

## Website and tool discovery

The horde.sh landing page lives in `website/`. See its [development and hosting
instructions](website/README.md) for the Next.js build and static export.

The site also publishes the agent-facing description of Horde: Horde discovery
metadata at `/.well-known/mcp.json`, the tool catalog at
`/.well-known/tools.json`, and `/llms.txt`. The discovery file uses Horde's own
format and describes the installed stdio MCP server; it is not an official MCP
manifest or an HTTP MCP endpoint. The site exposes no API of its own. The tool
catalog is generated from `src/protocol.rs`; regenerate and commit it with
`UPDATE_WEBSITE_SPEC=1 cargo test --test website_spec` whenever the operation
set changes, or `cargo test` will fail.

Contributions are licensed under Apache-2.0.
