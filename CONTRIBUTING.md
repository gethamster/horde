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

Contributions are licensed under Apache-2.0.
