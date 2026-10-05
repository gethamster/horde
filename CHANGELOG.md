# Changelog

## Unreleased

## 0.6.33 - 2026-10-04

- Accept Git's repeated credential capability and HTTP challenge fields so native
  Git operations authenticate with current Git versions. Keep duplicate origin
  and project fields rejected.
- Recover verified agent commits after a failed step leaves a stale Git index,
  preserving the commit and attempt history through the audited recovery API.
- Retain bounded, redacted Codex failure evidence and report provider diagnostics
  without recording authentication messages. Preserve capacity classification.
- Let native execution use an explicitly configured loopback Git bridge, with
  credentials restricted to that origin and the assigned project path. Preserve
  the existing Docker bridge default and captured execution-profile authority.

## 0.6.32 - 2026-10-04

- Support verified native archive previews for System, with a distinct signed
  checkpoint domain and explicit consumer capability negotiation. Preserve OCI
  checkpoint signatures and the existing review, admission, and recovery flow.
- Let operator policy select the host Docker socket and loopback registry
  publication endpoint while retaining canonical verified image digests.

## 0.6.31 - 2026-10-03

- Let an administrator correct a stopped Run’s unsupported model from the current
  project configuration before implementation begins. Preserve its identity,
  account permissions and failed attempts, record the original settings and
  correction atomically, and require a separate explicit resume. Refuse active or
  uncertain work, accepted results and immutable execution selections.

## 0.6.30 - 2026-10-02

- Preserve acknowledged operator feedback in a recovered attempt for its original
  worker and step. Include exact message and acknowledgement identities without
  resending messages, changing receipts, or exposing another worker’s context.
  Reject feedback that exceeds the bounded recovery context instead of silently
  dropping its intent.

## 0.6.29 - 2026-10-02

- Let Foundry show committed file changes from an exact verified Run checkpoint.
  Keep repository selection with the server, reject stale checkpoint refs, and
  bound source previews without reading uncommitted files or following symlinks.
- Preserve a later failed attempt when a caller retries a lost resume response
  with the same request ID. Persist the response with the original step reset.
- Expose operator feedback acknowledgements and checkpoint event ordering so
  Foundry can associate updated source snapshots with a new verified checkpoint.
  Keep message bodies and arbitrary source references out of this evidence.

## 0.6.28 - 2026-09-30

- Give automatic preview reviewers the authorized browser feedback for their
  Run, with later feedback taking precedence over conflicting original
  requirements. Preserve feedback history and exact source review safeguards.
- Package the verified release binary into an existing digest-pinned local
  tooling image without rebuilding its compiler, publishers, or runtimes.

## 0.6.27 - 2026-09-30

- Add Sign in with ChatGPT for native Horde workers. Account owners receive a
  browser consent URL, and Horde stores and renews the resulting credentials
  privately. Identity sign-in and permission to use a ChatGPT plan remain separate.
- Run native workers through the public Responses API with full conversation
  history, account-specific model selection, and streamed completion checks.
  Usage-limit errors pause requests without switching to paid API credentials.
- Transfer a protected login to a remote daemon over SSH, giving the destination
  sole refresh ownership. Sign-out stops local tool execution and reports whether
  remote revocation was confirmed.

## 0.6.26 - 2026-09-30

- Register existing Foundry project repositories through the private setup API.
  Validate strict typed requests and immutable project bindings, atomically
  persist repository/runtime grants with their success receipt, and reconcile
  the original operation after interruption. Registration preserves revoked
  grants and task-bearing projects, requires quiescent execution, and grants no
  accounts, preview publication, browser acceptance or release authority.

## 0.6.25 - 2026-09-30

- Derive signed release schema bounds from each compiled binary's state-free
  compatibility metadata. Check all platform records before signing, compare the
  extracted binary with the verified manifest before installation, and require
  the published binary to pass its own signed release compatibility check.
  Correct the schema-10 manifest bound published with the schema-11 0.6.23 and
  0.6.24 runtimes.

## 0.6.24 - 2026-09-30

### Added

- Optional administrator-owned GitHub contribution synchronization through WALGIT. Runs combine current WALGIT main and imported GitHub base at preparation boundaries, hold missing imports and conflicts, and validate the resulting exact head before checkpoint publication.
- Named WALGIT remote support for allocation, live base reads, reconciliation, and accepted Run publication. Legacy origin-only projects keep their existing behavior; GitHub credentials remain with Releases.

## 0.6.23 - 2026-09-30

- Keep drain and replacement held for uncertain attempts and unreconciled preview
  reservations. Match scheduler capacity accounting and confirm expired or
  released admission leases without replaying publication or discarding history.
- Export structural attempt failures, worker interruptions and held previews from
  the controller's durable journal to an explicitly configured private Signals
  scope. Frozen outbox identity survives lost responses and restart; rejected
  observations remain held for operator reconciliation. A separate native
  observation-exporter service holds the private transport credentials and checks
  every database policy/payload against immutable installation scope. The worker
  controller captures journal intent without reading credentials or sending HTTP.
- Expose bounded, project-scoped Run diagnostics with safe failure categories,
  credential metadata and reservation counts, without transcripts, objectives,
  credential material, raw errors or checkpoint signatures.

## 0.6.22 - 2026-09-29

- Verify controller drain responsiveness during active preview validation with
  explicit startup and phase checks. Scheduling delays in a busy CI runner no
  longer fail the test before its bounded validation process finishes.
- Wait for the bounded branch-push race fixture to reconcile its held result
  before shutting down its queue. A delayed remote hook still preserves the
  published checkpoint and detects the advanced protected main.

## 0.6.21 - 2026-09-29

- Keep release privacy checks deterministic when a cooldown timestamp or public
  identifier happens to contain the fixture card suffix. Credential and nested
  card metadata rejection remain covered by explicit regressions.

## 0.6.20 - 2026-09-29

- Build verified previews automatically for projects with an enabled setup policy.
  Delivery integrates current main before a real agent review, validates the exact
  tree, reserves publication capacity, verifies the native publisher receipt, signs
  the checkpoint, and publishes the Run branch. Acceptance and release remain user
  decisions.
- Preserve job, review-attempt, reservation, and retry identities through restart
  and lost responses. Changed heads, policy, failed validation, or admission pressure
  hold publication with an actionable status. Existing published digests can be
  reused when new publication is denied.
- Record the first enabled policy's activation cutoff so historical completed Runs
  do not launch unsolicited reviews. Preview work counts toward drain capacity;
  bounded subprocesses keep private APIs responsive during validation.

## 0.6.19 - 2026-09-29

- Configure execution profiles, workspace placement, storage, and account pools
  through a dedicated authenticated setup API. Durable operation receipts let
  installers inspect interrupted requests without blindly repeating changes.
- Restore component-owned execution profiles in native Rust, including scoped
  Git credential handling, instead of requiring installation-side scripts to
  edit runtime internals.

## 0.6.18 - 2026-09-27

- Let explicitly trusted local Docker installations run managed Codex with
  full access inside the worker container so agents can commit Git worktrees.
  The default sandbox and project account policies remain unchanged.

## 0.6.17 - 2026-09-27

- Keep task-wide messages sent by feedback follow-up agents in the worker
  mailbox without scheduling recursive follow-ups. Direct messages and later
  operator feedback can still wake the targeted worker for another revision.

## 0.6.16 - 2026-09-27

- Let an operator unpin an idle Run from an exhausted subscription after another
  authenticated account is granted to its project. The existing Run branch and
  steps remain intact; a durable receipt makes lost-response retries safe.

## 0.6.15 - 2026-09-27

- Recover agent-authored edits left in a failed local Run step with explicit
  operator validation. Horde commits the tested tree with worker and attempt
  provenance, integrates it on the same branch, and resumes skipped dependent
  work. Head changes and failed checks hold the recovery for renewed review.
- Upgrade existing schema 7 stores with the recovery receipt table before a
  failed-step recovery runs.

## 0.6.14 - 2026-09-27

- Resume a failed Run through skipped dependent steps so repair continues on the
  same branch. Completed work and unrelated conditional skips retain their state.
  If selected work changes before resume commits, recovery holds without
  resetting an active attempt or partially applying the retry.

## 0.6.13 - 2026-09-27

- Integrate the current main branch into a durable Run with explicit head checks.
  Conflicts retain the previous checkpoint and record repair evidence.
- Bind checkpoint signatures to the tested Git tree, expected main head, and an
  already-built image and build identity. Changed or omitted identities cannot
  reuse the same checkpoint idempotency key.
- Publish a local Docker runner from committed source with persistent Cargo
  caches. A sandbox-owned Docker daemon supports development and Compose tests
  without mounting the host daemon socket.

## 0.6.12 - 2026-09-24

- Route Horde's Linux CI, release, and website jobs to dedicated x64, ARM64,
  and small runners in `horde-fleet`. Keep macOS jobs on their existing hosts.
  The pre-cutover canary checked Git worktree support, disk capacity, Rust,
  Docker, and PostgreSQL on each pool.

## 0.6.11 - 2026-09-23

- Request worker cleanup and stop admitting work when host disk space falls
  below a configurable warning threshold. Suspend owned process groups at
  critical pressure, preserve their attempts and remaining execution budgets,
  and resume when enough space returns. An optional host cleanup command runs
  independently of paused workers. Hosts advertise no available fleet capacity
  while under pressure. Docker containers and in-flight remote requests are not
  suspended by these controls.
- Reclaim old, clean Horde worker checkouts from successful tasks while retaining
  their branches and integrated results. Cleanup preserves dirty workspaces,
  ignored caches, artifacts, and recovery data; removed checkouts can be restored
  for later work. Administrators can inspect storage health, configure policy,
  and preview cleanup through MCP or the CLI.
- Wait for observed process progress in storage-control tests and ensure test
  processes are reaped on failure. This release includes the disk-management
  changes from the unpublished v0.6.10 tag.

## 0.6.9 - 2026-09-23

- Add a `network` setting for Codex executors. With `network = true` on a
  provider or role, Horde passes `-c sandbox_workspace_write.network_access=true`
  (and sets the same option for managed Codex accounts). A Codex step can then
  fetch dependencies, for example crates for `cargo check`. Network access stays
  off by default, and settings without the key serialize exactly as before.
  Only user, project, and administrator configuration can grant it. A
  repository configuration file that would give a role network access is
  rejected.

## 0.6.8 - 2026-09-23

- Make Claude executor steps return their completion object reliably. The
  harness passes a JSON schema to Claude Code and reads the validated
  `structured_output`, so a final message written as prose no longer fails the
  step. A reply without the object gets one repair turn in the same session
  before the attempt fails.

- Compress repository snapshots sent to remote runtimes. Snapshots are now
  gzip-compressed tars carried as base64, limited to 24 MiB compressed and
  256 MiB expanded, so repositories whose uncompressed archive exceeds 24 MiB
  can be delegated. Runtimes advertise the new `snapshot_gzip` capability; older
  peers still receive and send the uncompressed form, which every runtime accepts.

## 0.6.7 - 2026-09-22

- Add `--human` to `horde answer` so an operator can answer a `human_only`
  question after a person gives the answer. Without the flag, the refusal now
  says to rerun with `--human`.
- Add administrative `provider_wallet` onboarding for a Link wallet. Its
  supervised installer uses the host's Node/npm to provision a pinned Link CLI, guides
  device login through Link's verification URL and phrase, and directs
  payment-detail work to Link Wallet without receiving card numbers or CVCs.
- Create and fund a Tuara account through an authorized Stripe Link wallet,
  then verify and install its inference key without copying it through the agent.
  Signup records the accepted terms and charge cap, survives restarts, and keeps
  uncertain payments on hold instead of charging again. The new administrative
  `provider_signup` operation supports quote, wallet approval, status, and recovery.
- Set up signup and automatic top-ups through guided CLI prompts, without writing
  JSON. Balance monitoring funds an existing Tuara organization within approved
  per-charge and monthly limits, including fees. Spending history survives policy
  changes and restarts, and unresolved payments hold further charges.
- Support Stripe Link test credentials for Tuara signup and top-ups. Parse Link's
  device-login events and Tuara's live MPP challenge shape so test-mode quotes
  and wallet connection work through the same MCP flow.

## 0.6.6 - 2026-09-22

- Accept Jev score legends returned as indexed objects as well as arrays,
  preserving exact label and probability checks. Live Tuara validation now
  covers routing and plan, patch, integration, and final reviews.
- Add an opt-in decision smoke test that waits for the final review, retains
  failure evidence, and requires actual provider responses before passing.
- Explain Responses, Chat Completions, Messages, and Decisions on the website,
  including which executor uses each API and how to configure Jev through Tuara.
  Preserve comparison tables in the generated Markdown documentation.
- Run project workers and Docker workloads on experimental AX runtimes, with
  project-bound storage, private routing, and suspend/resume support.
- Diagnose fleet enrollment failures and provide a one-command Taildrop invite.
  Expand setup guides for project-scoped work and deploying a parent with one child.

## 0.6.5 - 2026-09-21

- Run separate projects with their own repositories, operator configuration,
  runtime grants, and optional Lima isolation while sharing managed provider
  accounts. Task ownership and project-scoped capability evidence stay fixed
  across scheduling, delegation, review, and recovery.
- Add a provider-neutral decision-model service using the SystemOne protocol,
  with Tuara as the default provider and Jev as the launch model. Operators can
  opt into advisory routing and work-product reviews; each request uses the
  task's project settings and eligible runtimes, and cancelled tasks cannot
  send another provider request.
- Add separately opted-in native context pruning and browser tests. Both keep
  bounded evidence, validate responses, and fall back to conventional behavior
  when authorization or evidence is unavailable.
- Add automatic delivery gates that require exact held-out qualification and
  independent checks before a decision-model veto can affect a merge. Automatic
  delivery is limited to the default project; manual delivery drafts stay in
  the task's project workspace.

## 0.6.4 - 2026-09-21

- Add or replace provider API keys through a connected agent without terminal
  access. Explicit replacements apply to the next invocation, including when the
  daemon inherited an older key, and preserve other accounts and task settings.
- Sign in to Codex or Claude through agent-mediated browser handoffs. Sessions
  survive client disconnects, accept requested authorization codes, and verify
  authentication before reporting success. Cancellation, expiry, and daemon
  recovery stop interrupted login processes.
- Connect Tuara through its key page and submit an inference API key through the
  same handoff. Horde verifies inference access before saving the key, preserving
  the current model and role assignments without requiring a Tuara CLI.
- Invalidate affected provider quota observations after an account change while
  preserving local budgets. Discard late observations from the previous
  credentials; login verification does not claim available model quota.

## 0.6.3 - 2026-09-17

- Ship the default skills pack as `horde-skills.tar`. The public release
  repository's verifier names every artifact `horde-<target>.tar`, so the
  0.6.2 release (which introduced `skills.tar`) was signed and delivered but
  never published at horde.sh; installers select the pack by its `skills`
  target, not its file name, so nothing else changes.

## 0.6.2 - 2026-09-17

- Rule step-budget exhaustion on a fresh workspace sample taken at the deadline,
  with the baseline captured before the budget clock starts. The supervisor used
  to trust the last background poll to finish, so on a loaded runner a command's
  write inside the idle slack went unseen and the step was stopped; this failed
  the 0.6.1 release run on `ubuntu-24.04-arm` (`tests/budgets.rs:263`), so 0.6.1
  was never published (#56).
- `horde init`'s smoke check uses the daemon-client request budget instead of a
  private 500 ms socket deadline that #54 did not raise (#56).

## 0.6.1 - 2026-09-17

- Order the daemon-socket deadlines strictly inward (caller 30 s, daemon read 15 s,
  nested probe 5 s). The 1 s client read timeout was smaller than the daemon's own
  read deadline and than the nested `runtime_status` hop `agent_setup inspect` makes
  on the same socket, which made `inspect` report `daemon_unavailable` against a
  healthy daemon on a busy host and failed CI on the slowest runner (#54).

- Add a `grok` executor kind: the Grok CLI under its subscription login, prompt as an
  argument, coordination MCP server written to the workspace's `.grok/config.toml`,
  usable as a fallback hop after claude and codex.
- Start each task's integrated worktree from a freshly fetched `origin/<base>`
  instead of the operator's local `HEAD`, so pull requests no longer trail the
  base branch by however far the local checkout had drifted. Allocation fails
  when a configured `[delivery] base` cannot be fetched. Without a base, Horde
  fetches `origin` and prefers its default branch, falling back to local `HEAD`
  only when no remote tip resolves and recording a
  `workspace.local_head_fallback` event. The operator checkout is never
  modified; `workspace.integrated` and `workspace.registered` events record the
  resolved start commit and its source.
- Serialize a task's integrated worktree allocation per task, so parallel agent steps
  no longer race on `worktree add -b horde/<task>` and fail their dependents (#41).
- `horde validate` captures the skill catalog a submission would pin and fails on an
  unknown step skill; development binaries resolve the checkout's
  skills through symlinked paths.
- Let coding agents discover worker capabilities and resolve separate machine and
  model pools for thinking and delivery. Pin task-specific choices, inherit their
  limits, and reject unavailable or changed bindings without rewriting providers.
- Load default skills and injection metadata from files, with independent pack
  installation and authenticated worker synchronization. Binary updates and
  restarts accept named fleet-enrolled workers without SSH.
- Ship setup, discovery, model selection, planning, delegation, and review skills. Agents can
  propose project overrides, apply accepted changes, and review or restore history;
  existing tasks retain their captured skill versions.
- Expose setup actions and missing-access requirements to coding agents, including
  generic fleet startup through platform-managed secrets.
- Advance the database to schema 4 so older runtimes cannot ignore pinned
  execution constraints. Existing tasks and configuration migrate additively.
- Infer fleet key addresses and certificate names from controller configuration.
  Joining a named worker starts it and checks the authenticated connection while
  preserving unrelated existing runtime identities.
- Submit directly to a named worker with `horde submit --on`, inspect progress
  through the controller, and retrieve a separate review checkout with `horde result`.
- Show runtime names in terminal listings and support renaming or forgetting a
  disconnected entry without provider deletion or manual database edits.
- Deliver `task` broadcasts to the sender when it is the only worker, so a solo
  planner hears its own coordination messages instead of reaching nobody.
- Add `horde steer TASK_ID "message"` for operators. It posts as
  `operator:TASK_ID`, reaches every worker on the task including a single one,
  and wakes idle workers unless `--presence` is given. Pass `--worker ID`
  (alias `--to`) after `horde call list_workers` to target one worker instead
  of fan-out.
- Add `horde watch TASK_ID` and `horde events --follow`, an NDJSON stream of a
  task's durable events that ends with a `task.summary` line and exits 0, 1, or 2
  for succeeded, failed, or cancelled, 3 on `--timeout-secs`, and 4 when the
  daemon goes away. `--after SEQ` resumes a stream and `--interval-ms` sets the
  poll interval.
- Add `horde summary TASK_ID`, one JSON object with the status, step outcomes,
  branch, integrated head, and delivery outcome. Delivery reports `pr_ready` with
  the PR URL when `merge = false`, `merged`, `failed`, or `skipped` with an
  explicit reason such as `template has no delivery step` or `delivery disabled
  in settings`, mirrored in a top-level `delivery_skipped` field.
- Add an optional `[notify]` table that pushes `step.finished`, `task.finished`,
  `question.asked`, and `task.blocked` milestones to a webhook (`webhook` or
  `webhook_env`), a local command reading JSON on stdin, or both. A durable
  per-task cursor survives restarts, every attempt records `notify.delivered` or
  `notify.failed`, and a dead endpoint never stalls later events.
- Document the progress surfaces for scripts and agents in `docs/progress.md`.

## 0.6.0

- Mint fleet enrollment credentials with `horde network key create`. Workers
  enroll over TLS from containers, sandboxes, VMs, or individual machines without
  SSH, generate their own private keys, and renew their certificates automatically.
- Recover an expired worker certificate with the original valid fleet credential
  while keeping its identity and quota slot. Admission limits and revocation are
  enforced by the controller; fleet secrets stay outside saved worker state.
- List independently enrolled workers alongside managed runtimes, with heartbeat,
  expiry, and revocation status. Their launching platform retains lifecycle control.

- Add `complete_step` for native workers and remind planners to finish their
  assigned step after proposing work. Runtime acceptance checks still apply.

- Omit runtime identity and verification fields from worker tool schemas. Matching
  identity arguments remain valid; supplied verification flags are dropped with a
  warning, while identity mismatches still fail.
- Allow executor-role `extra_body` options to override native provider options,
  with reserved request fields rejected at configuration load.
- Resolve single-model catalogs with `model = "auto"`, list IDs when the catalog
  is ambiguous, and show the resolved model in `horde doctor`. Native providers
  can use only `base_url`, `api_key_env`, and `model = "auto"`.
- Include redacted tool arguments and results in events, with a configurable
  512-byte default limit per field and explicit truncation flags.

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
