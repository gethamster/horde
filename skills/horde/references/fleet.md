# Networking, managed runtimes, capacity, and updates

Everything here is optional. A single local daemon needs none of it.

## Connect your own machines

The fast path uses Tailscale. On the controller, with Horde installed:

```sh
horde network setup
horde network peers
horde network add alice@worker
horde runtime list
```

`network setup` installs Tailscale if needed (Homebrew is required on macOS),
prompts for sign-in, generates a private controller CA and identity, and starts
Horde. Stop an existing unconfigured daemon before the initial setup. Existing
manually configured trust is preserved and needs explicit migration.

The worker must already be reachable over Tailscale SSH with a non-root account.
On a Linux worker that already has Horde, prepare it with
`horde network setup --worker`. Otherwise install and connect Tailscale there and
enable its SSH server first. Tailnet policy must permit that SSH login and worker
connections to controller TCP port 7443; Horde does not change tailnet policy.

`network add` installs Horde from `https://horde.sh/install` when absent, sends a
unique certificate and enrollment packet over SSH, starts the remote daemon, and
waits for its authenticated outbound handshake. Repeating the same command reuses
the saved identity after an interrupted pairing.

Boot services: `network setup --service` on the controller,
`network add alice@worker --service` for the worker (needs remote passwordless
sudo).

Configure the worker's executors and provider authentication separately. SSH
pairing deliberately does not copy controller credentials or subscription logins.
Controller certificates last one year, worker certificates 30 days; renewal is an
operator-managed re-enrollment.

## The trust model

Both the `direct` and `tailscale` providers use tonic gRPC with rustls and
mandatory mutual TLS. Networking is disabled by default and configured separately
from project settings in `~/.config/horde/network.toml`. A repository `.horde.toml`
cannot change it. Start from `horde network config`.

Four independent things, none of which implies another:

1. **Certificate enrollment** — a client fingerprint in `allowed_clients`.
2. **Discovery** — being visible on the tailnet or in a peer table. Never a grant.
3. **Execution grant** — `delegate_peers` on the caller, `execution_clients` on the
   executor.
4. **Bundle grant** — `share_bundles` on the caller, `receive_bundles` on the
   executor.

Servers require both CA validation and a matching leaf fingerprint. Clients
validate the CA and the server's expected DNS name. There is no insecure mode and
no plaintext fallback. Store private keys mode 0600; Horde rejects group- or
world-readable keys.

```sh
horde network peers            # discovery, refreshed every call
horde network probe nEXAMPLE   # verify a peer's certificate, health, and execution permission
horde network listen           # foreground listener; horde start supervises it when configured
```

Do not run `network listen` on an address the daemon already uses. Restart
listeners after rotating certificates, changing enrollment, or switching tailnets:
configuration is read at startup.

Repository transfer across the network carries committed files only, not history
or untracked files. Archives are content-hashed, capped at 24 MiB compressed and
64 MiB expanded, and reject traversal, symlinks, special files, Git metadata,
tracked `.env`, and `*.key`. Those filename checks do not prove an arbitrary
repository is secret-free.

## Managed runtimes

Horde can provision execution hosts from user-owned profiles in
`~/.config/horde/runtimes.toml`:

```toml
[profiles.local-containers]
provider = "docker"
context = "default"
image = "ghcr.io/asomervell/horde@sha256:REPLACE_WITH_RELEASE_DIGEST"
concurrency = 4
cpus = 2
memory_mb = 2048
executor_roles = ["planner", "worker", "reviewer"]

[profiles.cluster]
provider = "kubernetes"
context = "my-cluster"
namespace = "task"
image = "ghcr.io/asomervell/horde@sha256:REPLACE_WITH_RELEASE_DIGEST"
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

For automatic enrollment, add these before any profile table:

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
