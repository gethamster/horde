import Link from "next/link";
import type { Metadata } from "next";
import InstallCommand from "./install-command";
import SiteHeader from "./site-header";
import { site, url } from "./site";
import { toolCount } from "./catalog";

export const metadata: Metadata = {
  title: { absolute: `${site.name} — ${site.tagline}` },
  description: site.description,
  alternates: { canonical: url("/") },
};

export default function Home() {
  return (
    <main className="home">
      <SiteHeader />
      <section className="hero">
        <h1>Horde</h1>
        <p className="tagline">{site.tagline}</p>
        <InstallCommand />
        <p className="hero-links">
          <Link href="/docs">Get started</Link>
          <a href={site.skills}>Agent skills</a>
        </p>
      </section>

      <section className="prose">
        <h2>What Horde does</h2>
        <p>
          Horde is a local Rust daemon that turns one objective into a durable workflow and
          coordinates coding agents against it. You submit a task — an objective plus a Git
          repository — and Horde plans it into steps, runs workers in isolated Git worktrees,
          records every event in SQLite, and integrates the result onto its own branch. Because
          the state is durable, execution continues after the submitting client disconnects, and
          interrupted work resumes instead of restarting.
        </p>
        <p>
          It is local-first by design. The daemon runs on your machine, uses the coding agent CLIs
          you already have installed and signed in to, and never pushes to your remote unless you
          enable delivery. Remote workers are opt-in: pair additional hosts over a Tailscale
          network and Horde schedules across them with per-host concurrency limits.
        </p>

        <h2>When to use Horde</h2>
        <p>
          Reach for Horde when a coding task is too large or too long-running for a single agent
          session, and you need the work to survive disconnects:
        </p>
        <ul>
          <li>
            <strong>Multi-step repository work</strong> — a change that needs planning,
            parallel implementation, and review, integrated onto one branch.
          </li>
          <li>
            <strong>Long-running jobs</strong> — work that outlives a terminal or chat session,
            with durable events you can inspect and resume later.
          </li>
          <li>
            <strong>Coordinating several agents</strong> — workers claim exclusive file paths,
            exchange messages, and share verified artifacts and knowledge instead of colliding.
          </li>
          <li>
            <strong>Driving Horde from your own agent</strong> — connect over the stdio MCP
            bridge and submit, inspect, and steer tasks as tool calls.
          </li>
        </ul>
        <p>
          Horde is not a hosted service and not a chat interface. If you want a single-shot edit
          in a file you already have open, use your coding agent directly.
        </p>

        <h2>How agents call it</h2>
        <p>
          Horde exposes {toolCount} operations over the Model Context Protocol. Add it as a stdio MCP
          server and your agent gets the full surface as typed tools:
        </p>
        <pre><code>{`{
  "mcpServers": {
    "horde": { "command": "horde", "args": ["mcp"] }
  }
}`}</code></pre>
        <p>
          If your agent supports the Agent Skills Protocol, the same workflow is packaged as
          three installable skills — one for driving Horde, one for authoring workflow templates,
          and one for agents running inside a Horde task:
        </p>
        <pre><code>{site.skillsInstall}</code></pre>
        <p>
          The catalog of those operations is published at{" "}
          <a href="/.well-known/tools.json">/.well-known/tools.json</a>, connection details at{" "}
          <a href="/.well-known/mcp.json">/.well-known/mcp.json</a>, and a summary for crawlers at{" "}
          <a href="/llms.txt">/llms.txt</a>. These describe Horde; they are not a service to call. Source code is available at{" "}
          <a href={site.repository}>gethamster/horde</a> under {site.license}.
          See the <Link href="/docs/agents">agent reference</Link> for the full list.
        </p>

        <h2>Requirements</h2>
        <p>
          macOS or Linux on Apple Silicon / ARM64 or x86-64, plus <code>curl</code>, Python 3, and <code>tar</code> for the installer. Running real work needs Git, ripgrep, and a
          configured coding agent CLI on each execution host. Releases are signed and the
          installer verifies the signature before installing.
        </p>
        <p>
          <Link className="next" href="/docs">Set up Horde →</Link>
        </p>
      </section>
    </main>
  );
}
