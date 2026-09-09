# Verification record

Development and execution checks were run on macOS arm64 with Rust 1.98.1. The CI workflow also targets Linux; Linux execution has not been run in this workstation session.

## Automated tests

The suite includes default automated tests and one opt-in Docker Compose test. `cargo test --locked` covers SQLite mailboxes and out-of-order acknowledgements, message idempotency, direct/group/task isolation, independent connection claim races, parent/child handoffs, worker token scope, artifact integrity and input-based reuse, knowledge provenance, template nesting/cycles/typed outputs, revision atomicity, approval and required-information questions, and recovery of interrupted attempts/worktree allocation.

Process tests launch real daemon, CLI, and MCP processes. They exercise client disconnection, mailbox recovery after daemon restart, cancellation of command process groups, live-process reconciliation refusal, bounded repair branches, explicit executor fallback, idle-worker wakeups, and a mocked Codex process committing and integrating real files.

Git tests perform actual clean merges, an actual conflict followed by worker repair, and independent edits that merge but fail a combined validation. Native tests exercise claimed writes, unified patches, symlink rejection, and out-of-scope integration holds.

Provider fixtures check exact and single-model `auto` resolution, ambiguous catalog
errors, doctor catalog reads, provider/role request options, fragmented SSE tool
calls, stable request history, usage, loop detection, and command timeout cleanup.
Worker tests cover schemas without runtime-owned arguments, matching identity
fields, dropped verification flags, and bounded redacted argument/result events.
A GitHub CLI fixture simulates losing the PR creation response; after daemon restart,
delivery finds the existing PR, watches checks, merges the verified head once, and
observes deployment.

`cargo fmt --all --check` and `cargo clippy --locked --all-targets -- -D warnings` are required checks.

Network tests cover tag-filtered Tailscale discovery, strict configuration, bounded CLI reads, disabled defaults, certificate enrollment, and real loopback gRPC mutual TLS. The installed Tailscale 1.96.3 client also passed a read-only discovery check: two local tailnet addresses and no `tag:horde` candidates. No devices were enrolled and no cross-host connection was tested. See [networking](networking.md) for enrollment and the restricted federation protocol.

## Delegation and app execution checks

Independent root and executor processes use real loopback mTLS, separate SQLite
stores, distinct user configurations, and CLI/MCP calls. The test verifies exact
submission retries, changed-assignment rejection, execution/caller authorization,
original constraints, an actual worker question answered through the root MCP
bridge, inherited app secrets, app readiness and testing, and parent integration.
The remote caller also creates a grandchild from its own committed workspace;
the original root counts it and records the remote parent's combined verification.
Temporary secret copies are checked for removal after completion.

Local tests exercise three delegation levels, the total child cap, caller-worker
authority, human-only escalation through each caller, source paging and retained
superseded constraints, stale context pins, additive schema upgrade, and rejection
of child acceptance after combined validation fails.

A real daemon is killed during a running app's test command. Restart stops both
owned process groups, removes the materialized `.env`, retains the uncertain
attempt, and preserves an unrelated process. A separate identity-mismatch test
holds cleanup without killing that process. Readiness failure, test failure, and
lifetime expiry also exercise cleanup. Shipping regressions also cover orphan app groups, failed Git imports, stale child acceptance after local/remote revisions, durable caller wakeups, consumed remote answers, legacy answer context, and responsive CLI/shutdown during slow peer discovery.

The opt-in Compose fixture passed on the workstation's existing `colima` context
with the cached `node:24-bookworm-slim` image. It starts a uniquely named HTTP app,
uses an owned named volume and ephemeral loopback published port, passes readiness
and an HTTP test, then verifies its containers, network, and volume are gone. No existing application,
Docker engine configuration, or tailnet policy was changed.

```sh
HORDE_TEST_DOCKER_CONTEXT=colima cargo test --locked --test environments real_compose -- --ignored --nocapture
```

Compose teardown after a hard daemon crash uses the persisted resolved project
configuration; the live Docker test covers ordinary lifecycle teardown, while the
hard-crash test covers process mode. Cross-host federation, provider calls across
the network, and arbitrary third-party Compose stacks were not tested live.

## Live provider checks

Both live harness smoke tasks used isolated temporary repositories, created `hello.txt`, verified its exact six bytes, committed it in their registered worktrees, and integrated it while leaving the original checkout unchanged.

| Executor | Result | Reported executor latency | Notes |
| --- | --- | --- | --- |
| Codex CLI | Passed | 35.3 s | Mailbox and claim tools worked after configuring this server's MCP tool approval mode. Reported 140,193 input tokens, 483 output tokens, and 121,472 cached input tokens. Subscription capacity/cost were not exposed. |
| Claude Code CLI | Passed | 36.3 s | Scoped MCP and allowed tools worked. CLI reported usage and a $0.48448425 cost estimate; this is not proof of a separately billed API charge on a subscription. |
| Tuara | Catalog and streaming tool calls verified live | — | `horde doctor --probe-tuara` against `qwen/qwen3.8-27b` on 2026-09-06 returned `catalog_verified` and `streaming_tool_calls_verified`, reading the key from `credentials.env` alone with nothing in the environment. A full `local-implementation` task on that default also ran live on 2026-09-06: the plan and implement steps succeeded and produced the requested file on the integration branch. Tuara returned intermittent `503 Service Unavailable` under that workload while single requests succeeded, which fails the step outright because retry and fallback are workflow policy, not executor behaviour. |

Tuara's [live model catalog](https://tuara.com/router/v1/models) listed the requested model as `z-ai/glm-5.3-flash` on 2026-09-05, and `qwen/qwen3.8-27b` — the default provider's model since — on 2026-09-06. The unauthenticated [instrument catalogue](https://tuara.com/api/v1/market/markets) quoted it at a best ask of 1.365 per million tokens on the same date. Neither listing exposes the upstream that serves a model, and the published [`ChatCompletionsRequest`](https://tuara.com/openapi.json) accepts only `model`, `messages`, `max_price`, `max_tokens`, `temperature`, `stream`, and `split`, so a buyer cannot pin a serving provider or a throughput floor. Its [chat-completions documentation](https://tuara.com/docs/buy/chat-completions/) specifies `/router/v1/chat/completions`, bearer authentication, string message content, streaming, and a decimal-string `max_price`. The code verifies the configured catalog identifier and never guesses an alias.

The Codex MCP adapter uses the supported [server approval configuration](https://developers.openai.com/codex/mcp/) for its supplied coordination tools. The installed `codex exec --help` and `claude --help` were also checked before live tests.

## Evaluation

`scripts/benchmark.py` compares a single worker in the configured planner role against a composed template on clones of the same repository. It records wall time, reported input/output/cache tokens, planner/reviewer usage, retries, messages, human answers, and missing-cost counts. Use `--verify 'python3 -m unittest' --preserve test_slug.py` to add independent validation and protect fixture tests.

An earlier recorded experiment measured a version predating 0.4.0, so its raw record has been retired rather than restated against a runtime it never ran on; re-run `scripts/benchmark.py` before citing these numbers. In that run both strategies passed six independent slugification tests and preserved the test file. The single-worker run took 57.3 seconds and 207,111 reported total tokens; the composed run took 112.2 seconds and 335,639 tokens. Coordination tools were called 5 and 7 times respectively, counted from captured Codex JSONL. Both required zero retries and zero human answers. API spend was not reported and remains unknown. It is one small coding workload, not a statistically meaningful claim of better model quality or lower cost. A run using Codex's default model for every role measures orchestration overhead; it does not establish savings from open-weight delegation. Tuara-backed efficiency comparisons remain dependent on credentials and representative workloads.

## Limits of this release

- No live GitHub PR, merge, deployment, or production health endpoint was configured or modified. External delivery was verified with a deterministic CLI fixture and a local Git remote.
- A hard crash during a model call or shell command is held for reconciliation. The runtime does not claim exactly-once arbitrary shell effects. Provider responses lost before persistence and unreported subscription limits remain unknown.
- Harness mail arrives through MCP reads and subsequent invocation prompts, not asynchronous injection into a running CLI session. Native workers check their mailbox between tool rounds.
- External harnesses and native arbitrary commands are cooperative execution. Their file changes are checked before integration; this is not complete prevention of malicious filesystem or network effects. Worker tokens are not an OS security boundary.
- Harness subscription/credential-store authentication was tested live. API-backed harness authentication uses a scoped loopback credential broker; its token, endpoint/model restrictions, HTTPS requirement and SSE passthrough were tested with local fixtures. Live API-backed harness billing/authentication remains untested without provider API credentials.
- The default local template performs plan, implementation and review. An assigned planner can call `propose_steps` to insert validated work before its pending successors; the personal agent can append revisions with `add_steps`. Free-form model prose never directly mutates authoritative workflow state.
- Artifact reuse is explicit and requires matching recorded inputs plus verification status. The service does not automatically infer a complete repository dependency graph or prove that caller-supplied fingerprints cover every input.
- Git metadata inspection/integration runs locally and synchronously. Custom repository Git hooks can extend these operations; fully cancellable hook/process reconciliation during integration is not covered by the process tests.
- Remote repository transfer is limited to committed file/directory snapshots; symlinks, special files, large archives, and tracked `.env`/`*.key` files are rejected. Each remote child reserves one root worker slot. There is no network-mounted SQLite or hostile-runtime isolation.
- App bundle redaction covers selected literal values. Source files and worktrees remain accessible to the same OS user. Process mode has lifecycle controls, not container CPU/memory enforcement.
- Worktrees, attempts and artifacts are retained. There is no automatic retention/garbage-collection policy, distributed scheduler, dashboard, or multi-user authorization layer.

These boundaries are explicit so mock coverage and a working early release are not confused with a production rollout across every external service in the plan.

## Runtime-management verification

The runtime-management additions are covered by temporary SQLite/repository tests,
loopback E2B/Daytona API fixtures, real loopback mTLS reverse-control enrollment,
management deduplication and permission tests, quota-window/fallback tests, live
scheduler-ceiling tests, service-definition escaping, and signed-manifest rejection.
`python3 scripts/test_release.py` verifies the generated installer offline with
a temporary Ed25519 key, including tampered metadata, corrupted archives, and
reinstall preservation. No test provisions a paid isolate or installs a boot service.

The runtime-management baseline passed 89 Rust tests with one opt-in Docker
Compose test ignored, before the later pairing and update-handoff tests were added.
Live E2B/Daytona provisioning, Kubernetes rollout, actual machine-boot behavior,
and a published-release rolling upgrade have not been exercised by this change.
Cloud template/snapshot startup and persistent storage must be configured as
described in `runtime-management.md`. Release CI requires its operator-supplied
signing key before publication. Claude subscription capacity remains unknown
when the installed harness supplies no structured quota data.

Automatic network setup tests exercise private certificate generation, stable
identity across retries, ambiguous/offline discovery rejection, real mTLS
enrollment with rejection of unenrolled certificates, administrative CLI scope,
and mocked remote installation that preserves bootstrap stdin. No test installs
Tailscale or changes a live tailnet. Live SSH pairing and boot-service setup remain
opt-in manual acceptance checks.

## Pinned skill checks

Tests load selected skill instructions once per attempt, read pinned resources,
and preserve the original bundle through restart and source-directory changes.
Child-task tests cover catalog narrowing and inheritance. Separate runtimes exchange
bundles over loopback mTLS after the sender's source directory is removed. Corrupt
hashes, traversal paths, symlinks, and modified materialized files are rejected.
These checks establish which instructions and resources were supplied; they do not
prove that a model followed them. See [runtime skills](runtime-skills.md).

## Native tool diagnostics

`horde events TASK_ID` includes `tool.completed` events for native tool and
coordination calls. Each records `attempt`, `success`, `duration_ms`, `error`
(null on success), and `error_truncated`, alongside the existing step, worker,
tool name, and timestamp. A failed call is evidence for diagnosis; it does not
by itself fail the step, and the worker still receives the tool response.

`arguments` and successful `result` values are redacted strings with separate
`arguments_truncated` and `result_truncated` flags. The top-level `tool_event_bytes`
setting defaults to 512 bytes per field; zero omits the payloads. `result_summary`
remains an alias of `result`. Failed calls have a null result and a separate error
summary capped at 2,048 UTF-8 bytes. A command can return normally while reporting
a nonzero exit code, so inspect its result as well as the event's `success` field.

Redaction removes selected application-bundle values and the active provider key
before encoding and truncation. If bundle values cannot be loaded, diagnostic
content is withheld. It covers known literal secrets, not arbitrary sensitive text.
These events cover the native executor; external CLI harness internals are not
captured. Older events may omit the new fields. See the
[native provider contract](native-providers.md#events-and-metrics) for usage and
provider telemetry.

## Running the smoke harness

Build the source binary first with `cargo build --locked`. The smoke harness uses
current named-provider configuration and a short temporary data path. It isolates
Horde settings from your normal config, disables delivery, and leaves your source
checkout alone. CLI runs retain your HOME and existing CLI login. API credentials
must be exported in the named environment variable; the harness does not read your
normal Horde `credentials.env` or write the key into configuration.

```sh
python3 scripts/live_smoke.py codex
python3 scripts/live_smoke.py claude
# Export TUARA_API_KEY first; choose an exact available catalog model.
python3 scripts/live_smoke.py tuara --model YOUR_MODEL_ID
# An unauthenticated local server can use a dummy key; use its real key otherwise.
HORDE_SMOKE_API_KEY=local-test python3 scripts/live_smoke.py local \
  --base-url http://127.0.0.1:8122/v1 --model YOUR_LOCAL_MODEL_ID
```

`local` selects the native executor (internally named `tuara`) against the given
endpoint. The endpoint must implement `/models` and native tool calls through
`/chat/completions`; this option is not a promise of compatibility with every
OpenAI-shaped server. No model identifier is silently substituted.

Use `--prepare-only` to load the configuration and compile the template without
starting a daemon or calling a provider. Use `--api-key-env NAME`, `--timeout N`,
`--max-tool-rounds N`, and `--binary PATH` to select credentials, bounds, or a build.
Native runs still require `--model` during preparation, and `local` requires an
explicit `--base-url`.

The printed temporary directory retains `daemon.log` and, when the daemon remains
available, `result.json`, `events.json`, and `metrics.json` for the task, including
on failure/timeout. These snapshots are taken before shutting down the owned
daemon. The harness verifies exact integrated file bytes and that the original
checkout has no hello.txt. It remains a one-step hello-world executor smoke, not
acceptance of the full planning/implementation/review workflow.

`python3 scripts/test_live_smoke.py` runs offline regressions against the built
binary: settings isolation, explicit endpoint/model requirements, and a local
mock provider that writes, commits, integrates, and produces retained evidence.
It does not call a real model, use a GPU, or publish changes.


## Workflow proposal contract checks

The Step schema is derived from the Rust types and inlined for MCP/native tools
and the website catalog. Offline fixtures exercise a native planner receiving a
field-specific `needs` error, correcting a proposal with nested `when` data, and
receiving a successful revision. Tests also check MCP error-result envelopes,
unknown/missing fields, nested environment type errors, unchanged workflow state
on rejection, and stable tool definitions across successive model requests.
These checks do not establish live pmlx planner convergence or prefix-cache speed.
