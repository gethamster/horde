# Runtime networking

Horde has optional `direct` and `tailscale` network providers. Both use tonic
gRPC with rustls and mandatory mutual TLS. Networking defaults to disabled.
The local daemon and SQLite store continue to use the private Unix socket.

The listener serves authenticated health and a restricted runtime federation
protocol. Execution additionally requires an explicit grant for the enrolled
caller. When networking is configured, `horde start` supervises the listener.
Managed workers connect outbound to the controller and do not need inbound ports.
Do not run `horde network listen` on an address already used by the daemon.

## Automatic fleet enrollment

A fleet credential lets workers join from Docker, Kubernetes, E2B, Daytona,
VMs, or individual machines. Each worker generates its own private key and
connects outbound to the controller. SSH is optional and is used only by the
separate remote installation command below.

Configure the controller's network identity first, using `horde network setup`
for Tailscale or the direct network configuration described below. Then create
a credential. Replace the example addresses and TLS name with reachable
controller addresses and a DNS name in its certificate:

```sh
horde network key create workers \
  --listen 192.0.2.10:7444 \
  --controller-address 192.0.2.10:7443 \
  --tls-name controller.example.com \
  --max-workers 100 \
  --output workers.json
horde start
```

`--listen` is a specific local address for the enrollment service, on a separate
port from the runtime listener. If a router forwards connections to this address,
set `--enrollment-address` to its public address. Both ports must be reachable
from workers. The existing runtime listener continues to require a worker
certificate; the enrollment listener exposes only registration and renewal.
Running controllers notice new enrollment settings within five seconds.

The output file has mode 0600 and contains the fleet secret, controller addresses,
and controller trust certificate. Existing output files are never overwritten.
The default credential permits 100 distinct workers to join over 30 days, with
four concurrent tasks per worker. Set `--expires-in` in seconds,
`--max-workers`, and `--concurrency` to change those limits. The worker limit
counts all identities ever admitted with that credential, including revoked
workers; retries with the same local identity do not consume another slot.

Supply the credential once through the platform that launches the fleet:

| Platform | Credential delivery |
| --- | --- |
| Docker | Mount a credential file readable only by the worker user; set `HORDE_ENROLLMENT_FILE` to its container path. |
| Kubernetes | Store the file in a Secret and expose its value as `HORDE_ENROLLMENT_JSON` through `secretKeyRef`. |
| E2B or Daytona | Supply `HORDE_ENROLLMENT_JSON` through the sandbox environment, or provision a private file and set `HORDE_ENROLLMENT_FILE` in the template's startup environment. |
| VMs | Use the image's secret delivery or startup configuration to provision a private file and set `HORDE_ENROLLMENT_FILE`. |
| Individual machines | Run `horde network join --invitation workers.json`, then `horde start`. |

For example, create a Kubernetes Secret in the worker namespace:

```sh
kubectl -n horde create secret generic horde-fleet --from-file=invitation=workers.json
```

Add this environment entry to the worker container specification:

```yaml
env:
  - name: HORDE_ENROLLMENT_JSON
    valueFrom:
      secretKeyRef:
        name: horde-fleet
        key: invitation
```

Every platform uses the same worker startup command:

```sh
horde --data-dir /data daemon
```

Set only one of `HORDE_ENROLLMENT_FILE` and `HORDE_ENROLLMENT_JSON`. File
credentials must be private regular files; mounted symlinks are supported when
their targets meet those requirements. Use the provider's restart policy or
the sandbox supervisor to retry an initial connection failure. Horde performs
a bounded enrollment attempt and retains its pending identity for the retry.
Preserve a separate data directory for each worker across ordinary restarts;
cloning an enrolled directory would also clone that worker's identity.

Workers retain their own keys and certificates without retaining the fleet
secret in enrollment state. Certificates last 24 hours and renew automatically
after 12 hours, including while the worker is connected. Restarts reuse the
saved identity without needing the fleet credential while its certificate is
valid. If the certificate expires, the worker automatically presents its fleet
credential again and proves ownership of its existing private key. Recovery
keeps the same identity and uses no additional enrollment slot. The fleet key
must still be valid, and the worker must not have been revoked.

Workers read the recovery credential from `HORDE_ENROLLMENT_FILE` or
`HORDE_ENROLLMENT_JSON`. A file-based join also remembers the original file path
for later recovery; keep that private file available. The secret itself is not
copied into worker state. If it is unavailable, restore the configured credential
source or run `horde network join --invitation workers.json` again. A disposable
container can instead start with a fresh data directory and enroll a new identity,
subject to the fleet key's remaining admission limit.
Provision model credentials separately through each worker's existing provider
configuration. Fleet enrollment does not copy the controller's API keys.

```sh
horde network key list
horde runtime list
horde runtime inspect WORKER_ID
horde network key revoke KEY_ID
horde network revoke WORKER_ID
```

Revoking a fleet key stops new admissions. Existing workers can still renew,
and a previously admitted worker can recover a lost registration reply while
its certificate remains valid. Revoking a worker stops renewal and readmission and closes its
control connection within the five-second authorization check. A connected
worker appears as ready only after it reports to the controller. Enrollment
does not give Horde ownership of the Docker container, sandbox, VM, or machine;
the platform that launched it remains responsible for its lifecycle.

## Remote installation over SSH

On the controller, with Horde installed:

```sh
horde network setup
horde network peers
horde network add alice@worker
horde runtime list
```

Setup installs Tailscale if needed (Homebrew is required on macOS), prompts for
Tailscale sign-in when needed, generates a private controller CA and identity,
and starts Horde. Stop an existing unconfigured daemon before initial setup.
Existing manually configured trust is preserved and requires explicit migration.

For `network add`, the worker must already be reachable through Tailscale SSH using a non-root account.
If Horde is installed on a Linux worker, prepare that access with:

```sh
horde network setup --worker
```

Otherwise, install and connect Tailscale there and enable its SSH server first.
Tailnet policy must permit the selected SSH login and worker connections to
controller TCP port 7443. Horde does not change tailnet policy.

`network add` installs Horde from `https://horde.sh/install` when absent, sends
a unique certificate and enrollment packet over SSH, starts the remote daemon,
and waits for its authenticated outbound handshake. Automatic download requires
a published release; source testing requires this build on both machines.
Configure the worker’s executors and authentication separately: SSH pairing does
not copy controller provider credentials or subscription logins. Repeating the
same add command reuses the saved identity after an interrupted pairing.

Use `network setup --service` for controller boot startup, or
`network add alice@worker --service` for worker boot startup. The latter requires
remote passwordless sudo. Without these flags, pairing starts the daemon without
installing a boot service. Worker preparation with `--worker` does not accept
`--service`; select that option from the controller during pairing.

Generated controller certificates last one year and worker certificates last
30 days. Renewal remains an operator-managed re-enrollment task. The remaining
sections describe manual configuration and direct connections.

## Tailscale provider

Automatic setup discovers all visible tailnet nodes as candidates; selecting a
host with `network add` authorizes pairing. Manual configurations default to
filtering `tag:task`; set `discover_all = true` to list other visible nodes.
Tag ownership and network grants remain under the tailnet administrator’s control.

Horde reads `tailscale status --json` to discover matching visible nodes, their
stable node IDs, MagicDNS names, tailnet IPs, and online status. Nodes outside
the configured filter, user records, public endpoints, and subnet routes are
excluded from its output.
Offline candidates remain visible; online status does not prove an Horde
listener exists. Discovery is refreshed on every `peers` and `probe` invocation.
It is limited to nodes visible to the local client, not a global service registry.

The provider uses the installed client for networking. It does not embed a
WireGuard implementation, change ACLs, enable Funnel/Serve, request auth keys,
or expose a public listener. `network setup` may invoke `tailscale up`; ordinary
discovery is read-only. Tailscale's official embedded
`tsnet` library is Go-based; a managed embedded sidecar is a possible future
provider implementation. The current provider requires host networking or a
TUN-enabled sidecar; userspace SOCKS-only networking is unsupported.

## Configuration and certificates

Automatic pairing stores trust in private `managed-network.toml` and certificate
files under the data directory. For manual setup, network trust is configured
separately from project settings in
`$XDG_CONFIG_HOME/horde/network.toml`, falling back to
`~/.config/horde/network.toml`. An explicit file can be selected with
`horde network --config /path/to/network.toml ...`. Repository `.horde.toml`
files cannot change this configuration. Relative certificate paths resolve
against the network configuration's directory.

Start with `horde network config`, then configure:

```toml
provider = "tailscale"
port = 7443
timeout_seconds = 10
tailscale_program = "tailscale"
discovery_tag = "tag:task"
ca_cert = "tls/ca.pem"
identity_cert = "tls/runtime.pem"
identity_key = "tls/runtime.key"

[allowed_clients]
# Replace with the lowercase SHA-256 DER fingerprint of an enrolled client cert.
"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" = "laptop-runtime"
```

Provision each runtime with its own private key and a certificate issued by your
Horde CA. Certificates used for both listening and probing need `serverAuth`
and `clientAuth` extended key usages. The server certificate's DNS subject
alternative name must match its complete MagicDNS name, without a trailing dot.
For manual enrollment, keep the CA signing key outside worker hosts. Automatic
pairing keeps its dedicated signer only on the controller. Store private keys with
mode 0600; Horde rejects keys accessible to group or other users.

Compute a client certificate's fingerprint for enrollment:

```sh
openssl x509 -in runtime.pem -outform DER | openssl dgst -sha256
```

Copy only the hexadecimal digest into `allowed_clients`. The mapped runtime ID
must match the peer ID used for routing: a direct peer table key or the stable
Tailscale node ID from `peers`. It is not a remote execution grant. Being on the tailnet, having `tag:task`, or possessing another certificate
from the same CA does not enroll a client. Servers require both successful CA
validation and a matching leaf certificate fingerprint. Clients validate the
configured CA and the server's expected DNS identity. There is no insecure mode
or fallback to plaintext.

```sh
horde network peers
horde start
# From another configured host, use the stable node ID printed by peers:
horde network probe nEXAMPLE
```

The Tailscale listener binds the first local tailnet address reported by the
client, normally IPv4. Probes try each candidate tailnet address in order with
a bounded timeout. No MagicDNS resolver is required for dialing: the provider
connects to the tailnet IP while validating the MagicDNS name in TLS. Peer ports
must match the configured port. Stop listeners with Ctrl-C or SIGTERM. Restart
listeners after rotating certificates, changing enrollment, or switching tailnets;
configuration is loaded at startup. Removing an allowlist entry takes effect
after restart, which also closes existing connections.

## Direct provider

For networks that already provide routing, use `provider = "direct"` and specify
a concrete local bind address (wildcard binds are rejected):

```toml
provider = "direct"
bind = "127.0.0.1"
port = 7443
ca_cert = "tls/ca.pem"
identity_cert = "tls/runtime.pem"
identity_key = "tls/runtime.key"

[peers.worker]
address = "127.0.0.1:7444"
tls_name = "worker.example.test"

[allowed_clients]
"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" = "laptop-runtime"
```

Use `horde network probe worker`. Direct peers have unknown online status
until probed. Direct mode has the same certificate and enrollment requirements.

## Enable bounded remote execution

Automatic pairing installs controller execution and management grants and uses
the outbound control connection. For manually connected peers, configure the
following reciprocal grants.

On the caller, add these top-level settings before TOML tables:

```toml
runtime_id = "laptop"
delegate_peers = ["worker"]

[share_bundles]
worker = ["app"]
```

On the executor, explicitly approve the caller and any app bundle transfer:

```toml
runtime_id = "worker"
execution_clients = ["laptop"]

[receive_bundles]
laptop = ["app"]
```

For direct RPC connections, both sides need enrolled client fingerprints and
reciprocal peer discovery, because execution calls go down and context/question
calls go back up. For
Tailscale, replace `worker` and `laptop` in routing and grant fields with the
stable node IDs. The descriptive `runtime_id` is reported by capabilities;
certificate enrollment identifies callers. Omit bundle tables when no app
secrets are needed. Each executor uses its own user-level model configuration
and provider credentials; caller provider credentials are never transferred.

```sh
horde start
# In another terminal / the original caller's CLI or MCP:
horde call delegate_task '{"task":"PARENT_ID","id":"remote-once","objective":"Implement export and test it","template":"local-implementation","peer":"worker"}'
horde call list_children '{"task":"PARENT_ID"}'
horde call integrate_child '{"task":"PARENT_ID","child":"CHILD_ID","validation":["npm","test"]}'
```

The daemon saves its effective configuration privately for outgoing calls.
Start the configured daemon before delegating. `probe` reports whether
that enrolled caller has execution permission.

Every task has one authoritative root runtime. Workers execute against their
own local stores and snapshots. Child IDs and assignment hashes deduplicate
submission retries. The caller reserves capacity until remote work and cleanup
finish. Temporary disconnection blocks new remote work and preserves reservations;
reconnection refreshes context and resumes only work blocked by that connection.
Hard-interrupted attempts retain the normal reconciliation requirement.

The internal protocol exposes capabilities, child acceptance/status/cancellation,
idempotent step revisions, context/question forwarding, environment leases, and
verified child snapshot exchange. The outbound control stream additionally
accepts explicitly granted runtime-management operations; it does not expose
arbitrary local admin calls.
Original questions cross unchanged and can be escalated through multiple callers.
The root bounds deeper delegation instead of giving every host a fresh budget.

Repository transfer includes committed files, not history or untracked files.
Archives are content-hashed, limited to 24 MiB archive payload and 64 MiB expanded entries, and reject traversal, symlinks, special
files, Git metadata, tracked `.env`, and `*.key`. These filename checks do not
prove an arbitrary repository is secret-free. App bundles use the separate
approved transfer path. Results require explicit parent integration and combined
validation; a remote success status alone is insufficient.

This is a cooperative single-user network of enrolled runtimes. It does not
provide a distributed scheduler, shared SQLite, transparent filesystem access,
public bot API, or an isolation boundary against malicious same-user code.

## Sources

- [Tailscale CLI status](https://tailscale.com/docs/reference/tailscale-cli): supported read-only JSON interface; its schema can change, so parsing failures are explicit.
- [Tailscale tsnet](https://pkg.go.dev/tailscale.com/tsnet): official embedded Go networking library.
- [Tonic transport](https://docs.rs/tonic/latest/tonic/transport/index.html) and [server TLS](https://docs.rs/tonic/latest/tonic/transport/server/struct.ServerTlsConfig.html).

Tests use simulated Tailscale CLI output and real loopback gRPC/TLS connections
with temporary certificate authorities. They cover valid enrollment, untrusted
and unenrolled certificates, missing client certificates, wrong server names,
private-key permissions, discovery filtering, malformed status, timeout, and
output limits. No tests enroll devices or modify a live tailnet.
