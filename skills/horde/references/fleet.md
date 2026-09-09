# Networking, managed runtimes, capacity, and updates

Everything here is optional. A single local daemon needs none of it.

## Set up a controller and workers through the agent

Use the calling agent's tools to complete the requested setup. Keep internal
ports, IDs, and configuration JSON in tool calls. Explain progress and blockers
in plain language instead of asking the user to assemble a networking recipe.

Start with `agent_setup` action `inspect`. Follow the relevant returned
`next_actions`: call the named tool for `kind:tool`, and run the supplied argv
through the agent's execution tool for `kind:exec`. Use the tool or execution API
that already reaches each target. An unchanged inspection will keep returning the
same unresolved checks, so evaluate their results before repeating the action.

Use `configure_provider` for missing requested providers or models. Presets are
convenient defaults; discover custom endpoints and concrete model names from
existing configuration or official provider information. The request supports
`kind`, `auth_mode`, `base_url`, `model`, `api_key_env`, and executor `roles`.
Provide credentials through `credential_env` or a private `credential_file`.
These fields refer to secret storage; never replace them with a credential value.
Subscription harnesses keep their own login stores on each machine.

Use `configure_controller` to preserve an existing controller identity or create
one through available Tailscale access. If private network access is missing,
follow the returned setup action using the caller's authorized platform tools.
A worker must be able to reach the controller; enrollment cannot create that
route or override a network policy. Existing direct networking is also supported.

Use `create_fleet_key` to save a private enrollment credential. Its `bootstrap`
response describes the daemon argv and a mode-0600 secret mount with
`HORDE_ENROLLMENT_FILE`. Use those returned values to configure worker startup;
do not expose the credential contents in a conversation or command argument.
On a machine that already has Horde, call `agent_setup` action `join_worker` with
the injected `invitation_file` and a human-readable `name`. The join flow preserves
an existing local runtime by selecting a separate worker data directory when
needed, then starts and checks the worker unless `no_start` was requested.

For E2B, Daytona, Docker, Kubernetes, or VM startup, inject that file through the
platform's secret facility and run the returned daemon argv. Each worker generates
its own private key and gets an individual certificate. Keep its data directory
across ordinary restarts, and never share one identity directory among replicas.
The credential's worker limit counts total distinct identities; replacing a
disposable worker with a fresh identity consumes another slot. Horde does not
create these external resources through `agent_setup`; use the caller's existing
platform access within the authorized scope.

A platform that injects only environment secrets may supply `HORDE_ENROLLMENT_JSON`
through that secret facility. Prefer `HORDE_ENROLLMENT_FILE` when available.
Neither mechanism requires someone to paste a key for each worker. Provider API
credentials remain separate from enrollment credentials and must be available to
the worker's selected executors.

Shared enrollment does not require SSH. Existing SSH access can still execute a
setup command on a machine, and legacy `network add user@host` remains an optional
installation path. Do not make editing Tailscale SSH policy a prerequisite for
workers that the agent can already reach through another execution API.

## Confirm what is ready

`agent_setup` distinguishes saved configuration from verified operation. A
`configured` result does not prove the provider accepts its credential, the
controller listener is reachable, or the selected model can execute. The `verify`
action currently performs inspection; `provider_api`, subscription authentication,
and listener checks reported as `not_probed` are still unverified.

Execute returned harness authentication-status checks and evaluate their actual
results. For API providers, report credential presence separately from successful
authentication or model execution. Confirm the worker's authenticated control
connection, then inspect `runtime_capabilities` and use `plan_execution` to resolve
the requested runtime/model pools. Check actual execution when required by the
task; do not describe configuration alone as a successful model test.

A failed or missing login may need the user's interactive authentication. Continue
independent authorized setup while that input is pending. Do not add a new
approval step to work already authorized, or silently create unrelated paid
resources to resolve a missing capability.

## Trust and renewal

Fleet admission uses a separate TLS endpoint. The worker validates the controller
using trust from the invitation and proves possession of its own key. The
controller validates the fleet credential and issues a client certificate with
its own runtime identity. The normal control connection requires mutual TLS and
an active enrolled identity. There is no plaintext or insecure fallback.

Admission credentials authorize joining a fleet, not controller administration.
Revoking a fleet key stops new admissions and expired-identity readmission;
existing members with valid certificates can still renew. Revoking a worker stops
its connection and renewal. Fleet certificates last 24 hours and renew after
12 hours. After expiry, a worker may reassert its still-valid fleet credential with
the same private key, retaining its identity and quota slot. An expired or revoked
fleet key cannot recover an expired worker certificate.

Execution authorization and secret-bundle sharing remain separate from discovery
and enrollment. A visible worker is not automatically permitted to run a task.
Keep selected runtime/capability pairs within the approved execution pools.
Provider-owned provisioning below uses a separate bootstrap path; its settings
and certificate lifetime do not describe shared fleet enrollment.

Repository transfer carries committed files, without history or untracked files.
Archives are content-hashed, capped at 24 MiB compressed and 64 MiB expanded, and
reject traversal, symlinks, special files, Git metadata, tracked `.env`, and `*.key`.
Those filename checks do not prove an arbitrary repository is secret-free.

## Managed runtimes

Horde can provision execution hosts from user-owned profiles in
`~/.config/horde/runtimes.toml`:

```toml
[profiles.local-containers]
provider = "docker"
context = "default"
image = "IMAGE_FROM_SIGNED_RELEASE_MANIFEST"
concurrency = 4
cpus = 2
memory_mb = 2048
executor_roles = ["planner", "worker", "reviewer"]

[profiles.cluster]
provider = "kubernetes"
context = "my-cluster"
namespace = "task"
image = "IMAGE_FROM_SIGNED_RELEASE_MANIFEST"
concurrency = 4

[profiles.e2b]
provider = "e2b"
api_key_env = "E2B_API_KEY"
image = "YOUR_HORDE_TEMPLATE_ID"
lifetime_seconds = 3600

[profiles.daytona]
provider = "daytona"
api_key_env = "DAYTONA_API_KEY"
image = "YOUR_HORDE_SNAPSHOT_NAME"
lifetime_seconds = 3600
```

Use the `image` value from the signed release manifest at
`https://horde.sh/releases/latest/manifest.json` in place of
`IMAGE_FROM_SIGNED_RELEASE_MANIFEST`; it includes the immutable image digest.

Container images must be pinned by digest. Kubernetes creates one StatefulSet and
dedicated persistent storage per runtime. E2B and Daytona templates must start
Horde and preserve their own runtime data across an ordinary restart: install an
official release with `install.sh --no-service`, include
`packaging/sandbox-start.sh`, and set that script as the template start command.
Provider API keys stay on the controller.

```sh
horde runtime create worker-1 --profile local-containers --request-id create-worker-1
horde runtime list
horde runtime inspect worker-1
horde runtime stop worker-1 --request-id stop-worker-1
horde runtime start worker-1 --request-id start-worker-1
horde runtime destroy worker-1 --request-id destroy-worker-1
```

Every operation persists its intent before contacting a provider. Reuse a
`request-id` only with identical arguments. A successful provisioning response
means `provisioned`, not `ready`: readiness is established by the authenticated
control connection. An ambiguous external result stays `uncertain` and Horde will
not blindly issue a second create. Inspect the provider, then adopt the exact
owned resource:

```sh
horde runtime reconcile worker-1 --resource task-worker-1 --request-id reconcile-worker-1
```

Reconciliation verifies the ownership label. It does not certify the effects of
interrupted tasks. Docker volumes and Kubernetes PVCs are retained on destruction
and must be removed separately.

For the provider-owned bootstrap path, the agent configures these values before
any profile table:

```toml
issuer_key = "/private/path/to/dedicated-horde-ca.key"
controller_address = "192.0.2.10:7443"
controller_tls_name = "controller.example.net"
```

Use a dedicated provisioning CA and never copy its signing key into a remote. Each
runtime gets a unique certificate, a bootstrap token valid for 15 minutes, and only
API credentials for its selected `executor_roles` (which must use API auth, Tuara,
or the simulated executor). Certificates expire after 30 days. Remotes connect
outbound and need no inbound ports; a dropped connection does not prove an
invocation failed or permit replay.

## Capacity

```sh
horde usage
horde call account_status '{}'
horde call account_observe '{"account":"shared-team-account","provider":"claude","window":"weekly","used_percent":85.0,"reset_at":1791158400,"observed_at":1788566400,"source":"provider"}'
```

Executors sharing `account = "name"` share capacity observations. Observations are
distinct from attempt token and cost metrics: they carry a window, observation
time, reset time, and a source of `provider` or `local_budget`.

Policy lives in `runtimes.toml`:

```toml
capacity_commands = []

[capacity_policy]
warn_percent = 80
switch_percent = 90
stale_seconds = 300

# [[budgets]]
# account = "shared-team-account"
# since = 1788566400
# reset_at = 1791158400
# usd = 25.0
# tokens = 1000000
```

At the switch threshold new invocations follow the configured role fallback chain.
No alternative provider is selected without that configuration, and if every
alternative is unavailable work stays queued. Unknown capacity permits execution.

Unknown is not zero and not unlimited. Stale observations are labeled stale.
Confirmed exhaustion with a future reset is held until that reset. Horde reads the
Codex app-server rate-limit API once a minute for recently used subscription
accounts, and consumes structured executor events and API broker response headers.
It does not scrape private web sessions or estimate subscription percentages from
token counts. Local budgets count Horde-reported usage only, not unrelated account
activity.

Custom collectors are argv arrays with no shell interpolation, each returning a
JSON array of snapshots:

```json
[{"account":"shared-team-account","provider":"claude","window":"weekly","used_percent":85.0,"reset_at":1791158400,"observed_at":1788566400,"source":"provider"}]
```

## Updates

```sh
horde skills install ./skills
horde runtime update apollo --skills
horde runtime inspect apollo
horde update --check
horde update --version 0.3.2
horde runtime update worker-1 --version 0.3.2 --request-id update-worker-1-021
horde runtime restart worker-1 --request-id restart-worker-1
horde call management_events '{"after":0}'
horde call management_ack '{"consumer":"my-agent","seq":12}'
```

A binary update verifies the signed manifest and artifact hash, stages a versioned
executable, drains active invocations, backs up SQLite, and atomically switches the
stable launcher. The replacement daemon verifies its expected version and
executable before resuming work.

A 30-minute drain timeout leaves the runtime draining and reports a blocked update;
`horde runtime resume` cancels that hold. Container updates replace the image while
preserving storage, and roll back only when the signed schema range establishes
compatibility. Where automatic rollback is unavailable, work stays held for an
operator. Package-manager and source installations must use their own installer.

Fleet updates run serially. A failed or uncertain update pauses the rest until you
inspect and release the hold:

```sh
horde call runtime_updates_resume '{}'
```

Wait for one runtime's update to reach `succeeded` before requesting the next. An
accepted remote command shows as waiting until the remote reports completion.

Skills update separately from the binary. `runtime_skills_update` takes a worker
name or ID and a stable request ID, captures the controller's default pack, and
sends it over the existing management connection. The result includes its hash;
running tasks and descendants keep their original pins. Binary updates and
restarts also support fleet-enrolled workers without SSH. Provider-owned containers
retain their image replacement path; independently launched hosts still need a
managed installation for binary self-update.
