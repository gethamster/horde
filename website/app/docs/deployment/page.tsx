import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../../site";

export const metadata: Metadata = {
  title: "Deploy: one parent, one child",
  description: "Connect one child worker to your Horde parent over Tailscale, send it a task, and retrieve the result. Start with two machines.",
  alternates: { canonical: url("/docs/deployment") },
};

export default function Deployment() {
  return <>
    <h1>One parent, one child</h1>
    <p>Your machine is the parent: you submit work and review the results there. A second machine is the child: it runs the agents. Horde calls these the controller and worker.</p>
    <p>Start with <Link href="/docs">Horde installed</Link> on both machines and both signed into the same Tailscale account. The child needs Git and its own <Link href="/docs/configuration">model provider configuration and login</Link>. Use versions of Horde that include <code>network invite</code>.</p>
    <h2>1. Invite the child from the parent</h2>
    <p>For first-time networking setup, run on the parent:</p>
    <pre><code>{`horde stop
horde network setup
horde network peers
horde network invite worker`}</code></pre>
    <p>Replace <code>worker</code> with your child’s hostname from <code>network peers</code>. Setup handles the parent’s Tailscale connection and Horde identity; automatic Tailscale installation on macOS needs Homebrew. If Horde networking is already configured, start with <code>network peers</code>.</p>
    <p>The invitation is sent through Tailscale’s Taildrop to your other device. It lasts one hour and admits one worker. This path needs Taildrop between your devices, not SSH. Your tailnet must allow the child to reach the parent’s runtime and enrollment addresses.</p>
    <h2>2. Join on the child</h2>
    <p>Receive the Taildrop file, then run the join command printed by the parent. For example:</p>
    <pre><code>{`horde network join ~/Downloads/horde-invite-worker.json`}</code></pre>
    <p>Use the file’s actual downloaded path and accept the transfer if Tailscale prompts. Join enrolls and starts the child, then waits for its authenticated connection. Invitation expiry does not disconnect an enrolled worker; see <Link href="/docs/deployment/fleet#certificates">certificate renewal and offline recovery</Link> for longer-term use. Provider logins are not copied from the parent.</p>
    <h2>3. Send it a task</h2>
    <p>Back on the parent, check that the child is ready. Use its runtime ID and a clean repository with your changes committed:</p>
    <pre><code>{`horde runtime list
horde --project default submit --on RUNTIME_ID --repo /path/to/repo "Run the tests and fix failures"
horde watch TASK_ID
horde result TASK_ID`}</code></pre>
    <p>Replace <code>RUNTIME_ID</code> with the child’s runtime ID and <code>TASK_ID</code> with the ID returned by submission. Wait for the task to succeed before retrieving its result. For this first task, use a repository that has not been assigned to a named Horde project. The command selects the default project explicitly.</p>
    <p>Horde sends the committed repository to the child. The result is a separate checkout on the parent for you to review; your original branch stays unchanged. If the child goes offline, its work waits rather than running on the parent.</p>
    <h2>When you need more</h2>
    <ul>
      <li><Link href="/docs/deployment/fleet">Manage a fleet</Link>: add more children, run at startup, update workers, and manage projects.</li>
      <li><Link href="/docs/deployment/ax">Use an existing AX deployment</Link>: run project workers in gVisor with optional Docker and Compose. Experimental.</li>
      <li><Link href="/docs/deployment/containers">Containers and sandboxes</Link>: configure Docker, Kubernetes, E2B, or Daytona workers.</li>
    </ul>
  </>;
}
