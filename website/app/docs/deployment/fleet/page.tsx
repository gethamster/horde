import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../../../site";

const guides = "https://github.com/gethamster/horde/blob/main/docs";

export const metadata: Metadata = {
  title: "Manage a fleet",
  description: "Inspect and update Horde workers, enable boot startup, and manage fleet enrollment, certificates, and project access.",
  alternates: { canonical: url("/docs/deployment/fleet") },
};

export default function FleetManagement() {
  return <>
    <p><Link href="/docs/deployment">Deploy workers</Link> / Fleet management</p>
    <h1>Manage a fleet</h1>
    <p>Use this reference after connecting your workers. For your first controller and worker, follow <Link href="/docs/deployment">Deploy workers</Link>. Each worker keeps its own identity, data directory, and concurrency limit.</p>
    <h2 id="ssh-pairing">Pair a child over Tailscale SSH</h2>
    <p>If your Linux child supports Tailscale SSH, Horde can install, enroll, and start it directly. On a child with Horde installed, stop its unconfigured daemon before first-time pairing:</p>
    <pre><code>{`horde stop
horde network setup --worker`}</code></pre>
    <p>Alternatively, prepare Tailscale SSH yourself. Use a non-root login and allow that SSH connection and outbound child connections to the controller on TCP port 7443 in your tailnet policy.</p>
    <p>On a parent with Horde networking configured:</p>
    <pre><code>{`horde network peers
horde network add alice@worker
horde runtime list`}</code></pre>
    <p>Replace <code>alice@worker</code> with the child’s login and hostname. Pairing installs Horde if absent and waits for its authenticated connection. Configure the child’s model providers separately. For devices without SSH access, use the <Link href="/docs/deployment">invitation walkthrough</Link>.</p>
    <h2 id="inspect-and-update">Inspect and update workers</h2>
    <p>Run these commands on the controller. Use an ID from <code>runtime list</code> and replace <code>VERSION</code> with a published Horde release:</p>
    <pre><code>{`horde runtime list
horde runtime inspect RUNTIME_ID
horde runtime update RUNTIME_ID --version VERSION --request-id update-worker-VERSION
horde runtime restart RUNTIME_ID --request-id restart-worker-1
horde call management_events '{"after":0}'`}</code></pre>
    <p>Reuse a request ID only to retry the same operation. Wait for an update to report <code>succeeded</code> before updating the next worker. Failed or uncertain updates pause further fleet updates; inspect the result before resuming them.</p>
    <p>Managed binary updates verify signatures and checksums, drain active work, preserve state, and restart. If the replacement cannot start and the service manager terminated its update helper, restore the previous launcher and restart the service. See <a href={`${guides}/runtime-management.md#updates-and-agent-events`}>update and recovery details</a>.</p>
    <p>Package-manager and source installations use their owning installer. Docker and Kubernetes updates use signed release images. AX workers require a new runtime with a new pinned image; see <Link href="/docs/deployment/ax">AX deployment</Link>.</p>
    <h2 id="boot-startup">Start workers at boot</h2>
    <p>For workers you enroll through Tailscale SSH, select service installation during initial pairing from the controller:</p>
    <pre><code>{`horde network setup --service
horde network add alice@worker --service`}</code></pre>
    <p>The first command enables controller boot startup. The second enables it on the selected SSH-paired worker and requires passwordless sudo there. Do not use SSH pairing to replace a worker already enrolled through an invitation. Without these flags, pairing starts the daemon without installing a boot service. Configure each paired worker’s executors and authentication separately.</p>
    <p>Containers and managed sandboxes use their deployment’s restart policy or a <Link href="/docs/deployment/containers#sandbox-supervisor">sandbox supervisor</Link>. Preserve one data directory per worker across restarts.</p>
    <h2 id="fleet-enrollment">Enroll workers without SSH</h2>
    <p>For workers started by your deployment tools, use <a href={`${guides}/networking.md#fleet-credentials-when-you-cant-ssh-in`}>automatic fleet enrollment</a>. A private fleet invitation lets each worker generate its own identity and connect outbound. Keep its recovery credential available and preserve its data directory; do not clone an enrolled worker’s directory to create another worker.</p>
    <h2 id="certificates">Certificate lifetimes</h2>
    <table>
      <thead><tr><th>Enrollment path</th><th>Worker certificate</th><th>Renewal</th></tr></thead>
      <tbody>
        <tr><td>Tailscale SSH pairing or provider provisioning</td><td>30 days</td><td>Operator-managed re-enrollment.</td></tr>
        <tr><td>Automatic fleet enrollment</td><td>24 hours</td><td>Automatic renewal after 12 hours. Expired-certificate recovery uses the retained invitation source and worker key.</td></tr>
      </tbody>
    </table>
    <p>Controllers generated by Tailscale setup have one-year certificates. A fleet worker’s recovery invitation must still be valid, and the worker must not have been revoked. See <a href={`${guides}/networking.md#fleet-credentials-when-you-cant-ssh-in`}>enrollment recovery and revocation</a> before removing credentials.</p>
    <h2 id="project-access">Project access</h2>
    <p>For project creation, repository registration, and provider accounts, start with <Link href="/docs/projects">Work in a project</Link>.</p>
    <p>Enrollment identifies a worker. Grant each project the workers it should use:</p>
    <pre><code>{`horde project runtime-grant PROJECT RUNTIME_ID`}</code></pre>
    <p>A manually configured receiver needs the same immutable project ID and matching execution grants. See <a href={`${guides}/networking.md#project-authorization-across-hosts`}>project authorization across hosts</a> for both sides of that setup, and <a href={`${guides}/runtime-management.md#project-capacity-and-credential-lifetimes`}>project capacity and credentials</a> for scheduling and account behavior.</p>
  </>;
}
