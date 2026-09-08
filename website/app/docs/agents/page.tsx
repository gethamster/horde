import Link from "next/link";
import type { Metadata } from "next";
import { site, url } from "../../site";
import { toolCount, tools } from "../../catalog";

export const metadata: Metadata = {
  title: "MCP and CLI reference",
  description: `Horde exposes ${toolCount} operations to agents over the Model Context Protocol. This page covers connecting an agent, the operation scopes, and the published descriptions of that surface.`,
  alternates: { canonical: url("/docs/agents") },
};

const workerTools = tools.filter((tool) => tool.scope === "worker");

export default function Agents() {
  return <>
    <h1>Horde MCP and CLI reference</h1>
    <p>
      Horde is driven by agents, not by an HTTP service. A running daemon exposes {toolCount}{" "}
      operations over the Model Context Protocol on stdio, and the same operations are available
      from the CLI. This site publishes a description of that surface so an agent can work out
      whether Horde is the right tool before anything is installed.
    </p>

    <h2>Connect an agent</h2>
    <p>
      Add Horde as a stdio MCP server. The bridge speaks protocol version <code>2024-11-05</code>{" "}
      and answers <code>tools/list</code> with typed schemas, so any function-calling client can
      use it without a hand-written adapter.
    </p>
    <pre><code>{`{
  "mcpServers": {
    "horde": { "command": "horde", "args": ["mcp"] }
  }
}`}</code></pre>
    <p>
      If the agent cannot find Horde, use the absolute path to <code>~/.local/bin/horde</code>.
      This bridge has administrative access to your daemon.
    </p>

    <h2>Agent skills</h2>
    <p>
      If your agent supports the Agent Skills Protocol, the workflow is packaged for you:{" "}
      <code>horde</code> for an agent driving Horde, <code>horde-templates</code> for authoring
      workflow templates, and <code>horde-worker</code> for an agent running inside a Horde task.
    </p>
    <pre><code>{site.skillsInstall}</code></pre>
    <p>Source and documentation: <a href={site.skills}>{site.skills}</a>.</p>

    <h2>Published descriptions</h2>
    <p>
      The discovery file uses Horde’s own metadata format. It describes the installed stdio
      server and is not an official MCP manifest or a remote connection URL. This site has
      no HTTP API, OpenAPI specification, or Streamable HTTP MCP endpoint.
    </p>
    <ul>
      <li><a href="/llms.txt">/llms.txt</a> — what Horde is and when an agent should reach for it</li>
      <li><a href="/.well-known/mcp.json">/.well-known/mcp.json</a> — Horde discovery metadata: transport, install, platforms</li>
      <li><a href="/.well-known/tools.json">/.well-known/tools.json</a> — all {toolCount} operations with JSON Schema parameters</li>
      <li><a href="/sitemap.xml">/sitemap.xml</a> — every indexable URL</li>
    </ul>
    <pre><code>{`curl -s https://horde.sh/.well-known/tools.json | jq '.tools[] | select(.scope=="worker") | .name'`}</code></pre>
    <p>
      The catalog is generated from the daemon source, so it always matches the release named in
      the manifest. Each entry carries a unique name, a description, and a closed JSON Schema for
      its arguments, which is the shape function-calling clients expect.
    </p>

    <h2>Markdown content negotiation</h2>
    <p>
      Every page on this site is also published as Markdown. Request it with an{" "}
      <code>Accept</code> header and you are redirected to the variant, or append{" "}
      <code>.md</code> to the path directly. Responses send <code>Vary: Accept</code> so a cache
      cannot hand an agent the wrong one.
    </p>
    <pre><code>{`curl -sL -H 'Accept: text/markdown' https://horde.sh/docs
curl -s https://horde.sh/docs.md`}</code></pre>

    <h2>Scopes</h2>
    <p>
      Operations split in two. <strong>Admin</strong> operations are available to your own MCP
      bridge and the CLI. <strong>Worker</strong> operations — {workerTools.length} of the{" "}
      {toolCount} — are additionally available to task-scoped worker credentials, which are
      issued per worker by <code>register_worker</code> and cannot read or affect other tasks.
      A worker token presenting an admin operation is rejected.
    </p>

    <h2>Calling operations from the CLI</h2>
    <p>
      Every operation is reachable without an MCP client through <code>horde call</code>, which
      takes the operation name and a JSON argument object and prints a JSON result:
    </p>
    <pre><code>{`horde call list_tasks '{}'
horde call submit_task '{"objective":"Add CSV export with tests","repo":"/path/to/repository"}'
horde call events '{"task":"TASK_ID","after":0}'`}</code></pre>

    <h2>Errors</h2>
    <p>
      Operations return an error rather than a partial result. Over MCP the failure arrives as a
      tool error; over <code>horde call</code> it is printed to stderr and the process exits
      non-zero. Common causes are an unknown operation name, an argument object that fails schema
      validation, a worker token attempting an admin operation, and a task ID that does not
      exist.
    </p>

    <h2>Worker-scoped operations</h2>
    <p>These are the operations a worker credential may call:</p>
    <ul className="columns">
      {workerTools.map((tool) => <li key={tool.name}><code>{tool.name}</code></li>)}
    </ul>

    <h2>Stability</h2>
    <p>
      Horde is an early release and operation names may change before 1.0. The catalog carries a{" "}
      <code>specVersion</code> and the manifest carries the Horde version it was generated from —
      check those rather than pinning to this page. Questions and issues go to{" "}
      <a href={site.issues}>the issue tracker</a>.
    </p>

    <Link className="next" href="/docs/configuration">Configure executors and concurrency →</Link>
  </>;
}
