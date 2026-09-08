import Link from "next/link";
import type { Metadata } from "next";
import Chrome from "./chrome";
import { site } from "./site";

export const metadata: Metadata = {
  title: "Page not found",
  description: "That page does not exist on horde.sh. Start from the documentation index or the machine-readable site map.",
  robots: { index: false, follow: true },
};

// Served with a real 404 status by the static host. The body is deliberately a
// recovery map: an agent that lands here should find the right URL without a
// second guess.
export default function NotFound() {
  return (
    <Chrome>
      <h1>404 — page not found</h1>
      <p>
        This URL does not exist on {site.domain}. Nothing was moved; the path is simply not part
        of the site. Use one of the entry points below.
      </p>

      <h2>Documentation</h2>
      <ul>
        <li><Link href="/docs">/docs</Link> — install Horde and run a first task</li>
        <li><Link href="/docs/agents">/docs/agents</Link> — connect an agent over MCP</li>
        <li><Link href="/docs/configuration">/docs/configuration</Link> — settings, concurrency, credentials</li>
        <li><Link href="/docs/deployment">/docs/deployment</Link> — remote workers over Tailscale</li>
      </ul>

      <h2>Machine-readable index</h2>
      <ul>
        <li><a href="/llms.txt">/llms.txt</a> — what Horde is and when an agent should use it</li>
        <li><a href="/sitemap.xml">/sitemap.xml</a> — every indexable URL</li>
        <li><a href="/.well-known/mcp.json">/.well-known/mcp.json</a> — MCP server manifest</li>
        <li><a href="/.well-known/tools.json">/.well-known/tools.json</a> — the full MCP tool catalog</li>
      </ul>

      <h2>Elsewhere</h2>
      <ul>
        <li><a href={site.releases}>Release manifest</a></li>
        <li><a href={site.skills}>Agent skills</a></li>
        <li><a href={site.issues}>Report an issue</a></li>
        <li><Link href="/contact">Contact</Link></li>
      </ul>
    </Chrome>
  );
}
