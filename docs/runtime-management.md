# Runtime management

Horde keeps a separate concurrency ceiling and SQLite store on every runtime.
A runtime is an execution host; a task and its delegated children retain
separate workflow identities. Personal agents use the same administrative tools
through CLI or MCP. Worker tokens cannot provision hosts, change concurrency,
report account capacity, or update binaries.

## Concurrency and usage

```sh
horde config get concurrency
horde config set concurrency 8
horde runtime status
horde usage
horde runtime drain
horde runtime resume
```

The default ceiling is four, with values from 1 through 64. An administrative
change persists in this instance's database and takes effect on the next scheduler
tick. Lowering it never kills active work. Without an override, the daemon reads
user configuration only; repository configuration can restrict its own task.
`HORDE_CONCURRENCY` supplies the initial ceiling in managed containers.

Executors may set `account = "shared-team-account"`. Roles referring to the same
account share capacity observations. Explicitly named accounts also share quota
observations across remotes. Implicit remote accounts are namespaced by runtime
so independent subscription logins are not accidentally combined.

Quota observations are distinct from attempt token/cost metrics. They carry a
window, observation time, reset time, and source (`provider` or `local_budget`).
Unknown capacity is not zero or unlimited. API rate windows are labeled separately
from subscription windows. Stale observations are shown as stale; confirmed
exhaustion with a future reset remains held until that reset.

For recently used Codex subscription accounts, the daemon reads the installed
app-server's `account/rateLimits/read` API once per minute. It never starts a
model turn. Structured Codex/Claude executor events and API broker response
headers are also consumed when they contain quota data. Claude versions that do
not expose account quota information remain unknown; Horde does not scrape
private web sessions or estimate subscription percentages from token counts.
Additional account collectors can be configured as argv arrays. Each must return
a JSON array of `Snapshot` objects; no shell interpolation is performed.

User-owned `~/.config/horde/runtimes.toml` (or the XDG configuration equivalent):

```toml
capacity_commands = []

[capacity_policy]
warn_percent = 80
switch_percent = 90
stale_seconds = 300

# Optional locally tracked budget, with explicit Unix-second window boundaries.
# [[budgets]]
# account = "shared-team-account"
# since = 1788566400
# reset_at = 1791158400
# usd = 25.0
# tokens = 1000000
```

Example collector output:

```json
[{"account":"shared-team-account","provider":"claude","window":"weekly","used_percent":85.0,"reset_at":1791158400,"observed_at":1788566400,"source":"provider"}]
```

At the switch threshold, new invocations follow the existing configured role
fallback chain. No alternative provider is selected without that configuration.
If every alternative is unavailable, work stays queued. Unknown capacity permits
execution. A failure after an invocation began retains the existing retry and
reconciliation rules; quota routing does not itself replay side effects. Local
budgets count Horde-reported usage, not unrelated account activity, and missing
usage remains unknown. They are not a strict fleet-wide billing guarantee.

Boot services preserve the selected absolute `XDG_CONFIG_HOME`.
They read API credentials from their daemon environment or a private
`credentials.env` beside `config.toml`, with mode 0600. This file is never injected
into arbitrary worker commands. Subscription authentication remains in each
installed CLI's credential store.

## Provisioning

Workers launched by your own deployment or autoscaler can register themselves
using [automatic fleet enrollment](networking.md#automatic-fleet-enrollment).
One credential works across all providers; each worker generates a separate
identity at startup. The provisioning profiles below remain available when
Horde should create and manage individual resources itself.

Configure a profile for each provider in `runtimes.toml`:

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

Docker uses the selected context. Kubernetes uses the selected kubeconfig context
and namespace, creating one StatefulSet and dedicated persistent storage per
runtime. Container images must be pinned by digest. E2B uses its sandbox API with
secure access and automatic pause; Daytona uses its sandbox API. Their templates
must start Horde and preserve their own runtime data on ordinary restart.
Provider API keys remain on the controller. For E2B/Daytona, install an official
release in the template with `install.sh --no-service`, include
`packaging/sandbox-start.sh`, and configure that script as the template start
command. This gives binary updates a supervisor without requiring systemd inside
an isolate. Docker/Kubernetes use the published OCI image directly.

```sh
horde runtime create worker-1 --profile local-containers --request-id create-worker-1
horde runtime list
horde runtime inspect worker-1
horde runtime stop worker-1 --request-id stop-worker-1
horde runtime start worker-1 --request-id start-worker-1
horde runtime destroy worker-1 --request-id destroy-worker-1
```

Operations persist intent before contacting a provider. Repeat a request ID only
with the same arguments. A successful provisioning response means `provisioned`,
not `ready`. Readiness is established by the authenticated control connection.
Ambiguous external results remain `uncertain`; Horde does not blindly issue a
second create. After inspecting the provider, adopt the exact owned resource:

```sh
horde runtime reconcile worker-1 --resource task-worker-1 --request-id reconcile-worker-1
```

Reconciliation verifies the ownership label. It does not certify the effects of
interrupted steps. Docker volumes and Kubernetes PVCs are retained on runtime
destruction for inspection and must be removed separately when no longer needed.

## Enrollment and outbound control

First configure the controller's mTLS network identity as described in
[networking](networking.md). The normal daemon supervises that listener when
networking is enabled. Do not simultaneously run a separate listener on the same
address. Use a user-managed reachable controller, including a routed tailnet
endpoint; remotes do not require inbound ports.

For automatic enrollment, add these top-level fields to `runtimes.toml`, before
any profile tables:

```toml
issuer_key = "/private/path/to/dedicated-task-ca.key"
controller_address = "192.0.3.00:7443" # Replace with the reachable address.
controller_tls_name = "controller.example.net"
```

The issuer key must correspond to the controller's configured CA and have mode
0600. Use a dedicated provisioning CA; never copy its signing key into a remote.
Each created runtime receives a unique certificate, a bootstrap token valid for
15 minutes, and only API credentials for explicitly selected `executor_roles`.
Selected roles must use API authentication, Tuara, or the simulated executor.
Certificates expire after 30 days; renewal is currently an operator-managed
re-enrollment task. Provisioning with no issuer key requires an already enrolled
runtime/template and an explicit `peer` in its profile.

The bootstrap packet is delivered through the provider environment (a Kubernetes
Secret or temporary private Docker env file for containers). Horde writes its
identity and configuration to private files before starting. After bootstrap,
restarts reuse that identity. Token hashes and public fingerprints are stored in
the controller database; credential values and private keys are not.

Remotes connect outbound over mTLS and reconnect after disconnects. The transport
carries correlated requests, replies, and heartbeats; workflow IDs and management
request IDs provide durable deduplication across reconnections. Heartbeats report
version, drain state, and quota observations. A dropped connection does not prove
an invocation failed or permit replay. Runtime destruction revokes its enrollment.

Manually enrolled remotes configure `controller_peer`; the controller needs a
matching `delegate_peers` grant. Remote `management_clients` grants are separate
from `execution_clients`. The local personal-agent bridge remains administrative.

## Updates and agent events

```sh
horde update --check
horde update --version 0.3.0
horde runtime update worker-1 --version 0.3.0 --request-id update-worker-1-021
horde runtime restart worker-1 --request-id restart-worker-1
horde call management_events '{"after":0}'
horde call management_ack '{"consumer":"my-slack-agent","seq":12}'
```

Binary updates verify the signed release manifest and artifact hash, stage a
versioned executable, drain active invocations, back up SQLite, and atomically
switch the stable launcher. A 30-minute drain timeout leaves the runtime draining
and reports a blocked update. Use `runtime resume` to cancel the hold. Failed
restart health checks restore the previous launcher for inspection/restart when
the update helper survives. The replacement daemon durably verifies its expected
version and executable before resuming work and completing the remote operation.
If the service manager kills the helper and the replacement fails before startup
completion, automatic rollback is unavailable: work remains held and an operator
must restore the previous launcher and restart the service.
Package-manager/source installations must use their owning installer.

Docker/Kubernetes updates select the signed release image digest, drain the
remote, replace the container image while preserving storage, and verify the
reported version before resuming work. Failed health checks restore the previous
image only when the signed schema range establishes rollback compatibility;
otherwise the runtime remains held for operator inspection. Interrupted controller
updates also pause subsequent fleet updates across restart. Cloud sandbox templates that use binary updates
must use the managed installer and a restart supervisor; an arbitrary unmanaged
binary is not overwritten.

Fleet operations execute serially. A failed or uncertain update pauses subsequent
updates; after inspection, use `horde call runtime_updates_resume '{}'` to
release that hold. A caller should wait for an update to reach
`succeeded` before requesting the next runtime's update. An accepted remote
command is shown as waiting until the remote reports completion. Events can be
consumed by an existing Slack bot or personal agent through MCP; no separate
Slack application or credentials are installed.

## Update a named worker

The calling agent can request a binary update or synchronize its current default
skill pack with a connected worker:

```sh
horde runtime update apollo --version 0.7.0
horde runtime update apollo --skills
horde runtime inspect apollo
```

Choose an available signed release version for the binary command. The CLI
returns a request ID; supply it through `--request-id` when reconciling a retry.
MCP calls use `runtime_update` with `id`, `version`, and `request_id`, or
`runtime_skills_update` with `id` and `request_id`. These operations also work for
independently enrolled fleet members, without an SSH connection or provisioning
profile. Names resolve to authenticated identities when the request is accepted.

Acceptance queues the request. Inspect that request's operation until it reports
`succeeded`, `failed`, or a condition requiring intervention. A binary update uses
the existing signed installer and drain procedure. Package-manager installations
and externally owned containers still require their installation or deployment
owner to replace the binary; enrollment alone does not grant that authority.

Skill synchronization activates a complete captured default pack without draining
or restarting. New tasks use it; running tasks, retries, and descendants retain
pinned instructions. Results and capability inventory report its content hash and
skill names. A worker must advertise `runtime_skills_update` support; older workers
need the runtime upgrade first. See [runtime skills](runtime-skills.md) for file
editing, metadata, installation, and project overrides.
