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
    <p>This walkthrough uses a Mac or Linux parent and one Linux child connected to the same Tailscale network. Start with <Link href="/docs">Horde installed</Link> on both machines. On the child, install Git and <Link href="/docs/configuration">configure and sign in to the model providers</Link> you want it to use. Pairing does not copy your parent’s provider logins.</p>
    <h2 id="prepare-the-worker">1. Prepare the child</h2>
    <p>For first-time pairing, stop any unconfigured Horde daemon on the child, then prepare its network:</p>
    <pre><code>{`horde stop
horde network setup --worker`}</code></pre>
    <p>Follow the Tailscale sign-in prompt and join the parent’s network. This prepares Tailscale SSH; use a non-root login such as <code>alice</code>. Your tailnet must permit that SSH login and connections from the child to the parent on TCP port 7443.</p>
    <h2>2. Connect from the parent</h2>
    <p>For first-time networking setup, run on the parent:</p>
    <pre><code>{`horde stop
horde network setup
horde network peers
horde network add alice@worker
horde runtime list`}</code></pre>
    <p>Replace <code>alice@worker</code> with the child’s login and hostname shown by <code>network peers</code>. Setup handles the parent’s Tailscale connection and Horde identity; automatic Tailscale installation on macOS needs Homebrew. If the parent already has Horde networking configured, start with <code>network peers</code>.</p>
    <p><code>network add</code> enrolls and starts the child, then waits for it to connect. Once <code>runtime list</code> shows it ready, copy its runtime ID for the next command. No runtime profile or container setup is needed.</p>
    <h2>3. Send it a task</h2>
    <p>Still on the parent, use a clean repository with your changes committed:</p>
    <pre><code>{`horde --project default submit --on RUNTIME_ID --repo /path/to/repo "Run the tests and fix failures"
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
