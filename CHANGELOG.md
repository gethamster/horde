# Changelog

## Unreleased

## 0.5.1

- Recover local schema-2 databases from before the Horde rename, preserving a
  full backup, task identities, records, and ownership. Unfinished work is held
  for inspection. Databases with federation records require manual migration.
- Add installer `--repair` to run the verified new updater when the installed
  updater fails before it can download its own fix.
- Accept Claude responses that include prose followed by fenced final JSON.

## 0.5.0

- Load pinned skills into workers and carry their files to child tasks. The
  additive database migration raises the schema to 3; release manifests support
  updates from schema 2, and the updater rejects binaries that cannot read schema 3.
- Preserve native provider conversation history, expose streaming progress, and
  stop repeated tool calls that make no progress.
- Replace the Claude writing-skill symlink with a regular-file entrypoint so
  remote repository snapshots can include it.

- Fix macOS Tailscale discovery during `horde network setup`. The Tailscale app
  now runs in CLI mode when Horde checks its status, avoiding a malformed JSON
  error caused by the app trying to launch its GUI.

- Add company copyright, Get Hamster, and Hamster Labs on X links to every website footer.

- Install all three agent skills directly from `gethamster/horde`, and find the guides through the README’s Documentation link and new documentation index.

- Publish the source at `gethamster/horde` under Apache-2.0 with a single initial commit and link it from horde.sh. Existing signed downloads keep their URLs.

- Make horde.sh missing-page responses recoverable as Markdown or JSON while preserving HTTP 404, clarify local MCP discovery, and publish the business address with improved developer resource links.

- Reserve an app environment's port until it has finished starting, so two
  environments starting at once cannot be handed the same one.
- Report a lost race for an app environment's port as such, instead of as a plain startup failure of the application.
- Declare each endpoint and API key once as a named provider, and let every executor
  role pick a provider and a model. `kind`, `auth_mode`, `base_url`, and `api_key_env`
  now belong to `[providers.<name>]`; a role setting one of them is rejected with the
  provider stanza to write instead. Providers inherit nothing from one another, so a
  provider missing the `base_url` or `api_key_env` it needs fails at load rather than
  silently borrowing another provider's endpoint or key.
- Default to Tuara over an API key for `planner`, `worker`, and `reviewer`, so a fresh
  install runs without a Codex or Claude subscription login. `codex` and `claude`
  providers and roles remain configured for those CLIs.
- Ask Tuara for `qwen/qwen3.8-27b` by default, replacing `z-ai/glm-5.3-flash`.
- Ask the native worker once for its result object when a final turn does not carry
  one. A model that answers in prose, or that leaves `content` blank because its
  thinking went to a separate field, had its step failed as malformed JSON; it is now
  asked for the object and only fails if the second reply also lacks it. A well-formed
  object declining the step is still a real answer and is not asked again. Failures
  quote what was actually received instead of reporting a bare parse position.
- Keep the stored key when `horde config provider add` only changes a model, instead
  of demanding it again; the walkthrough asks before replacing one.
- Add a provider and its key from the CLI, without hand-editing either file:
  `horde config provider add` walks through picking a provider, a model, and pasting
  the key, and takes flags for scripts; `horde config provider list` shows what is
  configured and whether each key reads; `horde config models <provider>` lists what
  that endpoint accepts. Presets cover Tuara, Codex, Claude, OpenAI, and Anthropic.
  The key is never a command-line argument — it is prompted for without echo or read
  from standard input — and is written only to `credentials.env`. Edits to
  `config.toml` are surgical, so comments and hand-written stanzas survive, and
  settings that would no longer load are reported instead of left on disk.
- Walk through providers during installation. `horde config init --interactive`, which
  the installer runs, offers to set up providers and keys one after another. It prompts
  on the controlling terminal rather than standard input, so it still asks under
  `curl ... | sh`, and prints the command to run later when there is no terminal at all
  instead of failing the install.
- Create the initial configuration on install: `horde config init` writes a commented
  starter `config.toml` and a private `credentials.env`, creating neither if it is
  already there, and the CLI installer runs it.

## 0.4.0

- Rename the top-level unit of work to a task, and the unit inside it to a step. SQLite tables and columns, the `submit_task`/`delegate_task`/`list_tasks`/`add_steps`/`propose_steps` operations, the `task` and `step` arguments, the `task.*` and `step.*` event kinds, and the `{{task}}` template input all change with it. Clients, stored event cursors, and databases from earlier versions are not compatible.
- Name the integrated result branch `horde/ID`.
- Remove compatibility with installations made before the Horde rename: the earlier configuration and data directories, environment-variable prefix, launcher alias and service identity, transitional container namespace, and release archive entry name.
- Verify a release after publishing it: poll horde.sh for the tagged version, install it as a user would, run it, and confirm the Linux binary needs no dynamic loader.
- Advertise the latest published release on horde.sh instead of the version in `Cargo.toml`, so the site cannot name a build that was never published.
- Redeploy horde.sh automatically once a release verifies, and reconcile daily if what the site advertises falls behind what is published.
- Detect a self-contained Linux binary by the absence of an ELF interpreter. `file` reports a `crt-static` musl build as `static-pie linked`, so the previous check rejected correct binaries.
- Add `horde`, `horde-templates`, and `horde-worker` agent skills, published from the public horde-skills repository.
- Publish the MCP tool catalog and server manifest on horde.sh under
  `/.well-known/`, generated from the daemon source.
- Serve every page as Markdown under `Accept: text/markdown`, with `Vary: Accept`.
- Add homepage content, `llms.txt` with when-to-use guidance, a sitemap,
  structured data, and about, contact, and privacy pages.
- Publish statically linked Linux binaries that run without a musl loader installed.

## 0.3.1

- Make installation self-contained on macOS and Linux.
- Add the launcher to a standard executable path automatically.
- Clarify Horde terminology in the user documentation.

## 0.3.0

- Rename the executable and distribution to Horde, with legacy configuration and managed-update compatibility.
- Add setup, configuration, and deployment documentation to horde.sh.
- Keep copy confirmation in the icon and add a simple Docs link to the homepage.

## 0.2.1

- Publish signed downloads through a public artifacts-only repository while keeping source private.
- Select OpenSSL 3 explicitly on macOS release runners and verify both macOS architectures before tagging.


## 0.2.0 — 2026-09-05

- Add automatic Tailscale setup, user-selected SSH installation/pairing, generated certificates, and verified outbound enrollment without manual fingerprint exchange.

- Add live per-runtime concurrency controls, account quota observations, local budgets, and capacity-aware executor fallback.
- Add E2B, Daytona, Docker, and Kubernetes management profiles, one-time mTLS enrollment, outbound control, and durable remote update/restart operations.
- Add signed versioned binary/image releases, an installer with machine-boot service opt-in, and drain-before-update behavior with compatible rollback while the update helper survives.
- Host the pinned public release key and installer at horde.sh, with signed artifact download routes backed by GitHub Releases.
- Complete remote binary updates durably from replacement daemon startup.

- Add optional Tailscale and direct discovery/network providers, user-owned network configuration, and CLI peer discovery.
- Add tonic/rustls mTLS runtime delegation with separate execution and bundle grants, idempotent submissions, committed snapshot exchange, and parent verification.
- Bound complete task trees, preserve original context and provenance across delegation, invalidate stale acceptance, and route questions through callers to the external root.
- Inherit named private app bundles and run disposable process or Docker Compose environments with readiness, tests, redacted evidence, and restart cleanup.
- Add additive SQLite schema migration, CLI/MCP coordination operations, and independent-process federation and crash-recovery tests.

- Keep caller wakeups and question receipts durable through revisions and restarts; reject stale child acceptance and failed Git imports.
- Keep CLI control responsive during peer discovery and cleanup; preserve legacy answers and reap orphan app groups.

## 0.1.0

Initial local runtime: durable SQLite workflows and revisions, CLI and stdio MCP,
worker mailboxes and scoped identities, atomic write ownership, isolated Git
worktrees, serialized integration, native Tuara tools, Codex and Claude adapters,
invocation-scoped API credential brokers, composable TOML templates, explicit
fallbacks and questions, knowledge provenance, content-addressed artifacts, and
configured GitHub/Actions delivery. Includes automated process/Git/provider tests,
live harness smoke scripts, and a baseline comparison runner.

This is an early release. Live Tuara inference and live GitHub delivery require
credentials/configuration not provided in the development environment. See
`docs/verification.md` for evidence and remaining limitations.
