import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../../../site";

export const metadata: Metadata = {
  title: "Containers and managed sandboxes",
  description: "Configure Horde workers in Docker, Kubernetes, E2B, or Daytona using runtime profiles and persistent worker storage.",
  alternates: { canonical: url("/docs/deployment/containers") },
};

export default function ContainerDeployment() {
  return <>
    <p><Link href="/docs/deployment">Deploy workers</Link> / Containers and sandboxes</p>
    <h1>Containers and managed sandboxes</h1>
    <p>Use these profiles when Horde should create and manage worker resources in an existing container environment or sandbox provider. Start with a configured Horde controller and a provider account or cluster you can already access.</p>
    <p>Configure Docker, Kubernetes, E2B, or Daytona profiles in <code>runtimes.toml</code>. Docker and Kubernetes require a digest-pinned image. Replace <code>IMAGE_FROM_SIGNED_RELEASE_MANIFEST</code> with the <code>image</code> value, including its immutable digest, from <a href="/releases/latest/manifest.json">the release manifest</a>.</p>
    <h2 id="docker">Docker</h2>
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
    <p>The issuer key must match the controller’s configured CA and have mode <code>0600</code>. Configure the selected roles and managed accounts on the controller. Horde provisions their authorized credentials to workers; ambient subscription logins are not copied.</p>
    <pre><code>{`horde runtime create worker-1 --profile containers --request-id create-worker-1
horde runtime inspect worker-1
horde runtime stop worker-1 --request-id stop-worker-1
horde runtime start worker-1 --request-id start-worker-1`}</code></pre>
    <h2 id="kubernetes">Kubernetes</h2>
    <p>For Kubernetes, use <code>provider = "kubernetes"</code> with a kubeconfig <code>context</code> and <code>namespace</code>. Each runtime gets a StatefulSet and persistent storage. Make sure the cluster can pull the configured image.</p>
    <h2 id="managed-sandboxes">E2B and Daytona</h2>
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
    <h2 id="sandbox-supervisor">Sandbox startup and persistence</h2>
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
    <p>For enrollment, updates, certificate lifetimes, and project access across several workers, see <Link href="/docs/deployment/fleet">fleet management</Link>. An existing AX deployment uses the separate <Link href="/docs/deployment/ax">AX guide</Link>.</p>
  </>;
}
