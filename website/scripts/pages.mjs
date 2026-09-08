// The single source of truth for the site's indexable pages.
//
// The sitemap, llms.txt, Markdown variants, and the Accept-header rewrites and
// Vary headers in both vercel.json files are all generated from this list, so a
// new page cannot be half-published.
export const origin = "https://www.horde.sh";
export const url = (path) => new URL(path, origin).toString();

/** `path` is the served URL; `file` is its location in the export. */
export const pages = [
  {
    path: "/", file: "index.html", markdown: "index.md", source: ["app/page.tsx"],
    priority: "1.0", changefreq: "weekly",
    title: "Horde — durable task orchestration for coding agents",
    summary: "What Horde is, when to use it, and the one-line install.",
  },
  {
    path: "/docs", file: "docs.html", markdown: "docs.md", source: ["app/docs/page.tsx"],
    priority: "0.9", changefreq: "weekly",
    title: "Set up Horde",
    summary: "Install, run a first task, and connect your own agent over MCP.",
  },
  {
    path: "/docs/agents", file: "docs/agents.html", markdown: "docs/agents.md",
    source: ["app/docs/agents/page.tsx"], priority: "0.9", changefreq: "weekly",
    title: "Horde MCP and CLI reference",
    summary: "Connect an agent over MCP, the operation scopes, and the published descriptions of that surface.",
  },
  {
    path: "/docs/configuration", file: "docs/configuration.html", markdown: "docs/configuration.md",
    source: ["app/docs/configuration/page.tsx"], priority: "0.8", changefreq: "monthly",
    title: "Configure",
    summary: "Settings files, executor roles, concurrency, account usage, and credentials.",
  },
  {
    path: "/docs/deployment", file: "docs/deployment.html", markdown: "docs/deployment.md",
    source: ["app/docs/deployment/page.tsx"], priority: "0.8", changefreq: "monthly",
    title: "Deploy remote workers",
    summary: "Pair a controller and workers over Tailscale, then inspect and update the fleet.",
  },
  {
    path: "/about", file: "about.html", markdown: "about.md", source: ["app/(site)/about/page.tsx"],
    priority: "0.5", changefreq: "yearly",
    title: "About Horde",
    summary: "What Horde is for, how it is built, and who maintains it.",
  },
  {
    path: "/contact", file: "contact.html", markdown: "contact.md", source: ["app/(site)/contact/page.tsx"],
    priority: "0.5", changefreq: "yearly",
    title: "Contact",
    summary: "Issue tracker for bugs, email for security reports and everything else.",
  },
  {
    path: "/privacy", file: "privacy.html", markdown: "privacy.md", source: ["app/(site)/privacy/page.tsx"],
    priority: "0.4", changefreq: "yearly",
    title: "Privacy",
    summary: "No analytics on the site, no telemetry in the daemon, no source leaves your machine.",
  },
];

/** Static documents published for agents. Descriptions of Horde, not a service. */
export const documents = [
  "/.well-known/mcp.json",
  "/.well-known/tools.json",
  "/llms.txt",
  "/llms-full.txt",
  "/sitemap.xml",
  "/robots.txt",
];

/**
 * Paths published before the API framing was dropped. They were live, so they
 * redirect rather than 404.
 */
export const retired = [
  { from: "/docs/api", to: "/docs/agents" },
  { from: "/docs/api.md", to: "/docs/agents.md" },
  { from: "/api/v1/tools.json", to: "/.well-known/tools.json" },
  { from: "/api/v1/index.json", to: "/.well-known/mcp.json" },
];
