import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../site";

export const metadata: Metadata = {
  title: "Set up Horde",
  description:
    "Install Horde, run your first task, and connect your own agent over the stdio MCP bridge.",
  alternates: { canonical: url("/docs") },
};

export default function Setup() {
  return <>
    <h1>Set up Horde</h1>
    <p>Horde is a local control plane for coding agents. It gives you one place to submit software tasks, assign them to configured agents, inspect their work, and connect your own tools through MCP.</p>
    <h2>Install</h2>
    <pre><code>{`curl -fsSL https://horde.sh/install | bash`}</code></pre>
    <p>The installer verifies the release, installs the <code>horde</code> command, and asks whether to start it at login.</p>
    <pre><code>{`horde --version
horde start`}</code></pre>
    <h2>Run your first task</h2>
    <p>Start with a Git repository that has an initial commit and a configured Git author. Horde gives each task its own worktree, so the submitted repository remains on its current branch.</p>
    <pre><code>{`horde submit "Add CSV export with tests" --repo /path/to/repository
horde inspect TASK_ID
horde events TASK_ID
horde metrics TASK_ID`}</code></pre>
    <p>Replace <code>TASK_ID</code> with the ID returned by submit. Changes are integrated onto a separate worktree. Your checkout stays on its branch; pushing is disabled by default.</p>
    <p>To exercise scheduling without model calls, use the simulated template:</p>
    <pre><code>{`horde submit "Exercise the runtime" --repo /path/to/repository --template simulated`}</code></pre>
    <h2>Connect your own agent</h2>
    <p>Add Horde as a stdio MCP server in your agent’s configuration. The same bridge can be used by an existing Slack bot or personal agent.</p>
    <pre><code>{`{
  "mcpServers": {
    "horde": { "command": "horde", "args": ["mcp"] }
  }
}`}</code></pre>
    <p>If the agent cannot find Horde, use the absolute path to <code>~/.local/bin/horde</code>. This bridge has administrative access; worker credentials use a separate, restricted bridge.</p>
    <p>The complete operation catalog, its JSON Schema parameters, and the published descriptions of that surface are in the <Link href="/docs/agents">agent reference</Link>.</p>
    <h2>Start, stop, and update</h2>
    <pre><code>{`horde runtime status
horde stop
horde update --check
horde update`}</code></pre>
    <p>The default data directory is <code>~/.local/share/horde</code>. Use <code>--data-dir /absolute/path</code> consistently for a different instance. Stopping the daemon keeps its durable state.</p>
    <Link className="next" href="/docs/configuration">Configure executors and concurrency →</Link>
  </>;
}
