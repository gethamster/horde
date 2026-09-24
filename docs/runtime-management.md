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
into arbitrary worker commands. Legacy subscription authentication in the default project remains in each
installed CLI's credential store. Managed accounts use the private profiles
and controller-owned refresh described below.

## Project capacity and credential lifetimes

A shared host may execute several projects, each with explicit runtime grants.
Use `horde project runtime-grant PROJECT RUNTIME` for each approved pairing and
`runtime-revoke` to withdraw it. Add `--dedicated` to bind the runtime permanently
to one project; revoke conflicting explicit grants first. This binding rejects
other project grants and excludes implicit `default` access. Newly managed
project runtimes receive this binding automatically. These project grants
supplement the host's network execution grants.

The scheduler rotates among eligible projects. Host and project concurrency
limits apply before dispatch, along with each account's shared limit. Account
selection prefers the lowest active-to-limit ratio, then the least recently
used account. Exhausted, expired, and unauthenticated accounts are excluded.
Configured fallback roles remain subject to their approved provider settings.

`horde --project PROJECT inspect TASK_ID` includes the project's queue reason
and each attempt's runtime, account, authentication profile, credential version,
and isolation mode. These bindings persist before execution. Uncertain local
attempts and unresolved remote reservations retain their capacity; another
account becoming available does not replay them.

Managed authentication uses separate project/account directories for harness
history and caches. API harnesses retain the invocation-scoped credential broker.
Codex subscription invocations use app-server external tokens; a private
controller profile serializes refresh and authenticated workers request access
tokens from it. Unsupported app-server authentication interfaces fail explicitly.
Claude receives the selected setup token with a separate `CLAUDE_CONFIG_DIR`.
A new project's execution cannot fall back to a default-project key or ambient
subscription login.

Credential deliveries retain their request ID and version for retries. Revocation
blocks new reservations and refresh, marks affected managed invocations for
cancellation, and requests removal of remote copies. An unreachable receiver
remains pending reconciliation. Inspect delivery state with
`horde --project PROJECT account delivery-list ACCOUNT_ID`; reconnect and
reconcile before treating remote cleanup as complete.

## Disk space and workspace retention

Each daemon watches free space on the data, project workspace, repository, active
worker workspace, and home filesystems. `watch_paths` adds other cache or build
volumes. By default, below 10 GiB Horde stops new admissions, advertises zero
available capacity, and sends each active worker one durable cleanup request.
Below 2 GiB it suspends the process groups it owns and holds subsequent tool
calls. Failed space probes also hold work. Recovery requires 14 GiB free to avoid
repeated stop/start cycles. These thresholds are configurable for large downloads.

A held attempt retains its identity and capacity reservations. Suspension does
not consume command timeouts or progress budgets. Cancellation still kills owned
process groups. When space recovers, the same attempt continues automatically;
Horde does not migrate a running process or resize the instance.

Inspect or configure the local policy through the administrative MCP tools or CLI:

```sh
horde call runtime_storage_status '{}'
horde call runtime_storage_configure '{"min_free_bytes":5368709120,"warning_free_bytes":42949672960,"resume_free_bytes":53687091200,"watch_paths":["/mnt/build-cache"]}'
horde call runtime_storage_configure '{"cleanup_command":["/usr/local/bin/horde-clean-disposable-caches"],"cleanup_timeout_seconds":60}'
horde call runtime_storage_cleanup '{}'
horde call runtime_storage_cleanup '{"dry_run":false,"limit":16}'
```

The cleanup command is optional, runs once per pressure incident in an independent
lane, and can reclaim known disposable caches while workers are suspended. Its
executable must be an absolute path. Horde supplies `HORDE_STORAGE_PRESSURE_FILE`
to the command and managed worker processes; that JSON file records host pressure
transitions. The command has a bounded timeout and its output is discarded;
status retains its outcome. An interrupted hook is not automatically rerun for
the same incident. Configure a command appropriate to the tools and caches on
that host. An empty `cleanup_command` disables the hook.

Worker cleanup requests are cooperative: workers act at a mailbox check, and a
suspended worker cannot perform cleanup until resumed. Arbitrary programs do not
automatically understand the pressure file. Only locally owned process groups
with verified live leaders are suspended. Docker containers keep running independently of their CLI; an
in-flight remote provider request also cannot be suspended, and its transport
timeout can still fail the attempt during a hold. Other host processes
can continue consuming disk, so these controls are not a filesystem quota.

Manual workspace cleanup defaults to a preview. Automatic maintenance runs once
a minute and considers a bounded batch of worker checkouts from successful tasks
older than seven days. It only removes Horde-owned, clean worker checkouts whose
commits are retained in the integrated task branch. Dirty, untracked, or ignored
files prevent removal. Active work and unresolved recovery records also prevent
cleanup. The integrated checkout, Git branches, artifacts, credentials, and task
history remain intact. A later invocation can recreate a removed worker checkout
from its recorded branch and commit. Horde does not automatically delete shared
build caches; the host cleanup command controls any such removal.

The policy belongs to the host administrator; project configuration and worker
tokens cannot change it. Each remote daemon enforces its own policy. Set
`automatic_cleanup` to `false` to disable periodic workspace cleanup; this does
not disable pressure monitoring or the separately configured hook. When omitted,
`warning_free_bytes` is 8 GiB above `min_free_bytes`, and `resume_free_bytes` is
4 GiB above the warning threshold. The status operation reports all watched
volumes, pressure state, process hold receipts, and cleanup outcomes.

After a daemon crash, held attempts become uncertain and require reconciliation.
Saved PID receipts never authorize automatic resumption. Inspect the process and
its effects before resolving the attempt. If the disk is already full, Horde
still attempts to stop verified live owned groups even when it cannot persist
the hold receipt, and reports that failure in daemon logs.

## Optional project VMs with Lima

Native execution works without Lima. On macOS and Linux, a `provider = "lima"`
profile provisions a Linux guest with its own Docker daemon and disk. macOS uses
Virtualization.framework; Linux requires KVM and uses QEMU. A project configured
with `--isolation vm` stays queued when no suitable guest is available.

Lima provisioning requires `limactl` 1.x or newer, a Linux Horde executable for
the host architecture, and a digest-pinned apt-based guest image. Configure the
usual controller enrollment signer and address, then add a profile to
`runtimes.toml`:

```toml
[profiles.horde-vm]
provider = "lima"
project = "PROJECT_UUID"
image = "DIGEST_MATCHING_LINUX_CLOUD_IMAGE_URL"
lima_image_digest = "sha256:IMAGE_SHA256"
lima_horde_binary = "/absolute/path/to/linux/horde"
lima_user = "horde_vm_project"
lima_home = "/var/lib/horde-lima/PROJECT_UUID"
lima_guard = "/usr/local/libexec/horde-lima-guard"
lima_egress = ["CONTROLLER_IP/32", "DNS_IP/32", "PACKAGE_MIRROR_CIDR"]
cpus = 2
memory_mb = 4096
disk_gb = 20
concurrency = 2
```

Replace every placeholder. The allowlist must cover controller communication,
DNS, and the selected image and package sources. The host administrator must
create a dedicated non-root OS user for each project's guests. Install
the output of `horde runtime guard-script` (also available at
`scripts/horde-lima-guard.py`) as the root-owned, mode-0755 executable above and
create a root-owned private `/etc/horde-lima/projects/PROJECT_UUID.json` containing
exactly `project`, `user`, `home`, and `egress`, matching the profile.

The daemon needs noninteractive sudo access to that guard's `apply` and `verify`
operations for the project, and to run `limactl` as its dedicated user. Linux
requires nftables. macOS requires enabled PF with `horde-lima/*` as the first
active filter anchor and no skipped interfaces. The guard validates this policy
and refuses mismatches; it does not replace the host's global firewall.

Guests use only Lima's `user-v2` network. Host-side rules restrict the dedicated
user's outbound connections. The guest receives no host-home mounts, SSH agent,
host Docker socket, or automatic forwarded ports. Provisioning installs Docker,
Compose, and Git, then enrolls the guest through authenticated Horde networking.
Broad egress grants broaden guest access, so list only the destinations needed
by that project.

On macOS, PF applies the dedicated-user policy to TCP and UDP. Lima's
[user-v2 implementation](https://github.com/lima-vm/lima/blob/master/pkg/networks/usernet/gvproxy.go)
uses gvisor-tap-vsock, whose [documented network limitation](https://github.com/containers/gvisor-tap-vsock#limitations)
prevents ICMP forwarding outside the virtual network. The sole `user-v2` NIC is
therefore part of the isolation requirement; adding another NIC invalidates it.
Linux uses an nftables UID rule that drops all other outbound protocols as well.
Run the opt-in acceptance suite on each supported host before relying on a new
Lima version in production.

Run `horde runtime doctor PROFILE` to verify the installed prerequisite and
host-network policy without provisioning a guest. Use the ordinary `runtime
create`, `inspect`, `start`, `stop`, `destroy`, and `reconcile` commands with the
Lima profile and its project selection:

```sh
horde --project horde runtime create horde-vm-1 --profile horde-vm --request-id create-horde-vm-1
horde --project horde runtime inspect horde-vm-1
```

Guest ownership and disk identity
cannot be reassigned to another project. Resource intent survives uncertain
management results; inspect the existing guest before reconciling it. Guest CPU
and memory reservations are checked against the provisioning host's resources.

Inspection reports the guest's actual resource name. New guests use compact
names to fit the host's SSH socket path limit; existing guests retain their
names and recorded configuration across upgrades. A failed create can be
destroyed using its saved ownership record, but an unavailable guest inventory
remains uncertain until the host can be inspected.

Guest setup retries failed package downloads a bounded number of times. A guest
becomes ready only after Docker and Compose work, its state directory exists,
and it enrolls with the controller. If setup fails, inspect
`/var/log/cloud-init-output.log` inside the retained guest before reconciling it.

To provision on another authorized host, set the controller profile's `host` to
that runtime's stable ID. Install the matching profile there without `host`, with
the same immutable project ID and local prerequisites. Both runtimes need the
project grants and management authorization. The guest connects directly to the
original controller. Native Docker still shares the host daemon; project naming
alone does not provide VM isolation. WSL uses native Linux execution; native
Windows execution is unsupported.

After preparing two project profiles and their host policies, run the live
acceptance suite explicitly on each macOS and Linux provisioning host:

```sh
python3 scripts/test_lima_live.py --allow-live \
  --project-a PROJECT_A_UUID --profile-a PROFILE_A \
  --project-b PROJECT_B_UUID --profile-b PROFILE_B \
  --blocked-ip REACHABLE_DENIED_IP --blocked-port DENIED_SERVICE_PORT
```

The denied service must be reachable from the host and excluded from both guest
allowlists. The suite creates temporary guests, checks Docker builds, Compose,
nested Docker, disk separation, network denial, and lifecycle retries, then
cleans up its resources. It requires installed Lima and prepared host policies;
the ordinary Rust tests use mocked providers and do not establish live VM
compatibility.

## Experimental AX workers

The `ax` provider runs project-bound Horde workers on Google AX and Agent
Substrate with gVisor. It uses the same runtime lifecycle commands and enrollment
as other backends. See [experimental AX runtimes](ax.md) for the pinned upstream
versions, runner image, profile configuration, and current limitations.

## Provisioning

Workers launched by your own deployment or autoscaler can register themselves
using [fleet credentials](networking.md#fleet-credentials-when-you-cant-ssh-in).
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
