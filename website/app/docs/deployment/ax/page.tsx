import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../../../site";

const reference = "https://github.com/gethamster/horde/blob/main/docs/ax.md";
const guides = "https://github.com/gethamster/horde/blob/main/docs";

export const metadata: Metadata = {
  title: "Use an existing AX deployment",
  description: "Connect Horde to an existing AX deployment, create project workers, and run tasks with optional Docker and Compose support.",
  alternates: { canonical: url("/docs/deployment/ax") },
};

export default function AxDeployment() {
  return <>
    <p><Link href="/docs/deployment">Deploy workers</Link> / AX</p>
    <h1>Use an existing AX deployment</h1>
    <p>Experimental. This guide assumes AX and Agent Substrate are already running. Horde connects to that deployment and creates workers in gVisor. Your Horde controller can stay on your local machine.</p>
    <h2>Before you start</h2>
    <ul>
      <li>Use AX revision <code>d8ed0fe38bceb7842d3c47817d53d16ccdfcb601</code> with its pinned Substrate dependency. For AX installation, follow <a href="https://github.com/google/ax/tree/d8ed0fe38bceb7842d3c47817d53d16ccdfcb601#quick-start">upstream AX documentation</a>.</li>
      <li>Make the AX gRPC API and Substrate HTTP router available to your controller through private, loopback-bound tunnels. Workers must be able to connect back to the controller.</li>
      <li>Use matching controller and runner builds from a Horde revision with AX support; older releases may not include it. Configure Horde’s <a href={`${guides}/runtime-management.md#enrollment-and-outbound-control`}>controller identity and enrollment signer</a>, and a <a href={`${guides}/configuration.md#separate-projects-and-provider-accounts`}>project with a registered repository and model accounts</a>.</li>
      <li>Use persistent AX and Substrate storage if workers must survive host restarts. See the <a href={`${reference}#prerequisites`}>storage prerequisites</a>.</li>
    </ul>
    <h2>1. Prepare a Horde runner image</h2>
    <p>The AX runner is currently built from source; Horde does not yet publish a prebuilt AX image. From a Horde checkout, place an Ubuntu 24.04-compatible Linux AMD64 Horde executable at <code>dist/amd64/horde</code> (see the <a href={`${reference}#build-the-runner-image`}>binary build instructions</a>), then build and push:</p>
    <pre><code>{`docker build --platform linux/amd64 -f containers/ax/Dockerfile \\
  -t REGISTRY/horde-ax:experimental .
docker push REGISTRY/horde-ax:experimental
docker inspect --format '{{index .RepoDigests 0}}' REGISTRY/horde-ax:experimental`}</code></pre>
    <p>Replace <code>REGISTRY</code> with a registry your AX workers can pull from. Keep the returned <code>image@sha256:...</code> reference for the next step. The image contains Horde, Git, and the model CLIs. If tasks need Docker or Compose, apply the <a href="#docker-and-compose">optional Docker setup</a> before building and creating the worker.</p>
    <h2>2. Add an AX profile</h2>
    <p>Add this to the controller’s <code>~/.config/horde/runtimes.toml</code>. Keep the enrollment settings at the top of the file, before any profile tables; reuse your configured controller CA and address.</p>
    <pre><code>{`issuer_key = "/absolute/path/to/controller/ca.key"
controller_address = "CONTROLLER_ADDRESS:7443"
controller_tls_name = "controller.example.net"

[profiles.ax-worker]
provider = "ax"
project = "PROJECT_UUID"
endpoint = "http://127.0.0.1:9090"
ax_router_endpoint = "http://127.0.0.1:8001"
ax_revision = "d8ed0fe38bceb7842d3c47817d53d16ccdfcb601"
image = "REGISTRY/horde-ax@sha256:IMAGE_SHA256"
ax_egress = ["*:443"]
cpus = 2
memory_mb = 4096
concurrency = 2
executor_roles = ["planner", "worker", "reviewer"]`}</code></pre>
    <p>Replace the placeholders and tunnel ports. Get the project UUID with <code>horde project inspect PROJECT</code>. The listed executor roles must exist in that project’s configuration. Configure credentials through Horde’s managed accounts; ambient CLI logins are not copied into workers. Use a separate profile for each project.</p>
    <p>The pinned AX gateway ignores the port in <code>ax_egress</code>, so <code>*:443</code> permits all outbound traffic. Use explicit hostnames or CIDRs to restrict destinations. CPU and memory settings are sent to AX; per-worker enforcement has not been established by this integration.</p>
    <h2>3. Create and verify a worker</h2>
    <pre><code>{`horde --project PROJECT runtime create ax-1 --profile ax-worker --request-id create-ax-1
horde --project PROJECT runtime inspect ax-1
horde --project PROJECT call runtime_capabilities '{}'`}</code></pre>
    <p>Replace <code>PROJECT</code> with your project slug or UUID. Wait for Horde’s authenticated worker connection to be ready before submitting work. AX reporting its Task ready is only the first part of startup. Capabilities should show <code>gvisor</code> isolation.</p>
    <h2>4. Run a task</h2>
    <pre><code>{`horde --project PROJECT submit --on ax-1 --repo /path/to/repo "Run the tests"`}</code></pre>
    <p>Use a registered repository with a clean checkout and committed changes. Horde transfers the repository and runs the workflow on <code>ax-1</code>. Work selected for that worker waits while it is unavailable; it does not fall back to your local machine.</p>
    <h2>Stop and resume</h2>
    <pre><code>{`horde --project PROJECT runtime stop ax-1 --request-id stop-ax-1
horde --project PROJECT runtime start ax-1 --request-id start-ax-1`}</code></pre>
    <p>Stop drains active work and retains the workspace and worker identity. Reuse a request ID only to retry the same operation; use a new ID for the next stop/start cycle. For image upgrades, create a new runtime with the new image digest. See <a href={`${reference}#recovery-and-upgrades`}>recovery and upgrades</a> for uncertain operations and cleanup.</p>
    <h2 id="docker-and-compose">Optional Docker and Compose</h2>
    <p>Docker and Compose work inside AX workers when the deployment supplies the required guest capabilities and gVisor settings. Follow the <a href={`${reference}#optional-nested-docker`}>AX Docker configuration</a>, which includes the pinned AX patch and sandbox setup, then build the Horde runner with <code>--build-arg ENABLE_DOCKER=true</code>.</p>
    <p>Create a new worker using that image and verify that <code>runtime_capabilities</code> reports Docker and Compose available. Each worker has its own Docker daemon and persistent image and volume storage; it does not use the host Docker socket. Builds and Compose service networking have passed live tests. Published ports (<code>-p</code>) remain untested.</p>
    <p>If Docker fails, Horde stays connected for inspection and draining. Explicitly stop and start the worker to recover Docker. See the <a href={reference}>full AX reference</a> for configuration details and tested limitations.</p>
  </>;
}
