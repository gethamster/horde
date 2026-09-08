import type { Metadata } from "next";
import { url } from "../../site";
export const metadata: Metadata = {
  title: "Deploy",
  description: "Pair a Horde controller with remote workers over Tailscale, then inspect and update the fleet.",
  alternates: { canonical: url("/docs/deployment") },
};

export default function Deployment() {
  return <>
    <h1>Deploy remote workers</h1>
    <p>To run agents on another machine, pair a Horde controller with a worker over your Tailscale network. The controller schedules work; the worker runs the configured agents with its own credentials and resource limits.</p>
    <h2>Prepare the worker</h2>
    <p>The worker must be reachable through Tailscale SSH with a non-root account. On a Linux worker with Horde installed, run:</p>
    <pre><code>{`horde network setup --worker`}</code></pre>
    <p>Alternatively, install Tailscale on the worker, connect it to your tailnet, and enable Tailscale SSH. Tailnet policy must allow your selected SSH login and outbound worker connections to the controller on TCP port 7443.</p>
    <h2>Pair from the controller</h2>
    <p>Stop an existing unconfigured daemon before first-time networking setup:</p>
    <pre><code>{`horde stop
horde network setup
horde network peers
horde network add alice@worker
horde runtime list`}</code></pre>
    <p>Replace <code>alice@worker</code> with the SSH user and discovered host. Setup installs Tailscale if needed, handles sign-in, and creates the controller’s certificate authority. macOS needs Homebrew for automatic Tailscale installation.</p>
    <p>Pairing installs Horde on the worker if absent, generates a unique certificate, and waits for an authenticated outbound handshake. Discovery lists candidates; selecting a host authorizes enrollment.</p>
    <p>Use <code>network setup --service</code> for controller boot startup. Use <code>network add alice@worker --service</code> for the worker; this requires passwordless sudo on that host. Configure the worker’s executor login separately.</p>
    <h2>Inspect and update</h2>
    <p>Use the runtime ID returned by <code>runtime list</code>:</p>
    <pre><code>{`horde runtime inspect RUNTIME_ID
horde runtime update RUNTIME_ID --version 0.3.0 --request-id update-worker-030
horde runtime restart RUNTIME_ID --request-id restart-worker-1
horde call management_events '{"after":0}'`}</code></pre>
    <p>Reuse a request ID only to retry the same operation. Wait for an update to report <code>succeeded</code> before updating the next host. Failed or uncertain updates pause the fleet; inspect the result before resuming it.</p>
    <p>Binary updates verify signatures and checksums, drain active work, and restart. If the replacement cannot start and its helper was terminated by the service manager, operator recovery is required. Worker certificates expire after 30 days; renewal currently requires operator-managed re-enrollment.</p>
    <h2>Containers and sandboxes</h2>
    <p>Horde also supports Docker, Kubernetes, E2B, and Daytona profiles in <code>runtimes.toml</code>. Docker and Kubernetes require a digest-pinned image. Replace <code>IMAGE_FROM_SIGNED_RELEASE_MANIFEST</code> with the <code>image</code> value, including its immutable digest, from <a href="/releases/latest/manifest.json">the release manifest</a>.</p>
    <pre><code>{`# Top-level enrollment settings; use your controller's CA and address.
issuer_key = "/absolute/path/to/controller/ca.key"
controller_address = "CONTROLLER_TAILNET_IP:7443"
controller_tls_name = "controller.your-tailnet.ts.net"

[profiles.containers]
provider = "docker"
context = "default"
image = "IMAGE_FROM_SIGNED_RELEASE_MANIFEST"
concurrency = 4
cpus = 2
memory_mb = 2048
executor_roles = ["planner", "worker", "reviewer"]`}</code></pre>
    <p>The issuer key must match the controller’s configured CA and have mode <code>0600</code>. Selected executor roles must use API authentication, Tuara, or the simulated executor. Provider credentials remain on the controller.</p>
    <pre><code>{`horde runtime create worker-1 --profile containers --request-id create-worker-1
horde runtime inspect worker-1
horde runtime stop worker-1 --request-id stop-worker-1
horde runtime start worker-1 --request-id start-worker-1`}</code></pre>
    <p>For Kubernetes, use <code>provider = "kubernetes"</code> with a kubeconfig <code>context</code> and <code>namespace</code>. Each runtime gets a StatefulSet and persistent storage. Make sure the cluster can pull the configured image.</p>
    <p>E2B and Daytona require a prepared Horde template or snapshot with persistent runtime data and a restart supervisor. They do not provision from an arbitrary blank image:</p>
    <pre><code>{`[profiles.e2b]
provider = "e2b"
api_key_env = "E2B_API_KEY"
image = "YOUR_HORDE_TEMPLATE_ID"
lifetime_seconds = 3600

[profiles.daytona]
provider = "daytona"
api_key_env = "DAYTONA_API_KEY"
image = "YOUR_HORDE_SNAPSHOT_NAME"
lifetime_seconds = 3600`}</code></pre>
    <p>Install the signed binary inside the template, configure its executor credentials, and use this supervisor as its start command. Keep its data directory on persistent storage:</p>
    <pre><code>{`curl -fsSL https://horde.sh/install | bash -s -- --no-service`}</code></pre>
    <pre><code>{`#!/bin/sh
# Use as the start command of an E2B template or Daytona snapshot after installing
# an official release with install.sh --no-service. Storage must survive restarts.
set -u
export HORDE_SUPERVISED=1
horde_data_dir=\${HORDE_DATA_DIR:-}
set --
[ -z "$horde_data_dir" ] || set -- --data-dir "$horde_data_dir"
child=''
stop() {
  if [ -n "$child" ]; then kill -TERM "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; fi
  exit 0
}
trap stop TERM INT
while :; do
  "$HOME/.local/bin/horde" "$@" daemon &
  child=$!
  wait "$child"
  result=$?
  child=''
  [ "$result" -ne 0 ] || exit 0
  sleep 2
done
`}</code></pre>
    <p>A provisioned resource becomes ready only after its authenticated controller connection is established.</p>
  </>;
}
