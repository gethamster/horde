// Post-build generator for the agent-facing surface of horde.sh.
//
// Everything here is derived from the built export so it cannot drift from the
// pages: Markdown variants come from the rendered HTML, and the sitemap and
// llms.txt come from the pages that were actually emitted. The MCP tool catalog
// is generated separately from the Rust source (`cargo test --test website_spec`)
// and is only read here.
import { readFileSync, writeFileSync, mkdirSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { execFileSync } from "node:child_process";
import { toMarkdown } from "./html-to-markdown.mjs";
import { pages, origin, url } from "./pages.mjs";

const root = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const out = join(root, "out");

const catalog = JSON.parse(readFileSync(join(root, "public/.well-known/tools.json"), "utf8"));

// The version this site advertises is the one an agent can actually install, so
// it comes from the published release manifest rather than from Cargo.toml.
// A source tree can sit ahead of the last release, and a release can fail after
// its version is committed; publishing the source version in either case tells
// agents to install something that does not exist. Fail the build instead.
const RELEASE_MANIFEST =
  process.env.HORDE_RELEASE_MANIFEST_URL ??
  "https://horde.sh/releases/latest/manifest.json";
const response = await fetch(RELEASE_MANIFEST, { redirect: "follow" });
if (!response.ok) {
  throw new Error(
    `Cannot read the published release manifest (${response.status} from ${RELEASE_MANIFEST}). ` +
      "Refusing to build a site that advertises an unverified version.",
  );
}
const version = (await response.json()).version;
if (!/^[0-9]+\.[0-9]+\.[0-9]+([-.0-9A-Za-z]*)$/.test(version ?? "")) {
  throw new Error(`Published release manifest has no usable version: ${JSON.stringify(version)}`);
}
const source = /^version = "([^"]+)"/m.exec(readFileSync(join(root, "..", "Cargo.toml"), "utf8"))?.[1];
if (source !== version) {
  console.warn(
    `Source tree is at ${source}; latest published release is ${version}. ` +
      "The site advertises the published version.",
  );
}

const write = (path, body) => {
  const file = join(out, path);
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, body);
  return path;
};
const json = (path, value) => write(path, JSON.stringify(value, null, 2) + "\n");

/** Last commit date for a path, so sitemap lastmod reflects real edits. */
const lastModified = (paths) => {
  for (const path of paths) {
    try {
      const date = execFileSync("git", ["log", "-1", "--format=%cI", "--", path], {
        cwd: root,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      }).trim();
      if (date) return date;
    } catch {
      // Not a git checkout (or the file is new): fall through to today.
    }
  }
  return new Date().toISOString();
};

// ------------------------------------------------- Markdown from built HTML

for (const page of pages) {
  const file = join(out, page.file);
  if (!statSync(file, { throwIfNoEntry: false })) {
    throw new Error(`export is missing ${page.file}; the page list is out of date`);
  }
  page.body = toMarkdown(readFileSync(file, "utf8"), origin);
  if (page.body.length < 200) {
    throw new Error(`${page.path} produced only ${page.body.length} characters of Markdown`);
  }
  page.lastmod = lastModified([...page.source, "app/layout.tsx"]);
}

// ------------------------------------------------------- Markdown variants

const generated = [];
for (const page of pages) {
  // The Accept-header rewrites in vercel.json map each page path onto these.
  const header = `# ${page.title}\n\n> ${page.summary}\n>\n> Source: ${url(page.path)}\n\n---\n\n`;
  const body = page.body.replace(/^# .*\n+/, "");
  generated.push(write(page.markdown, header + body + "\n"));
}

generated.push(write("404.md", `# 404 — page not found

This path does not exist on Horde’s website. Horde has no hosted HTTP API or
OpenAPI specification. Install Horde to use its CLI or local stdio MCP server.

- [Horde documentation](${url("/docs")}): install and run your first task.
- [Horde MCP and CLI reference](${url("/docs/agents")}): connect an agent locally.
- [Agent index](${url("/llms.txt")}): choose the right resource.
- [Sitemap](${url("/sitemap.xml")}): all published pages.
`));

// RFC 9457 Problem Details for agents requesting JSON from a missing URL.
json("404.json", {
  type: "about:blank",
  title: "Not Found",
  status: 404,
  detail: "This path does not exist. Horde has no hosted HTTP API or OpenAPI specification. Install Horde to use its CLI or local stdio MCP server.",
  code: "not_found",
  hint: "Start with the documentation index or llms.txt; use the sitemap to find published pages.",
  links: { docs: url("/docs"), llms: url("/llms.txt"), sitemap: url("/sitemap.xml"), agents: url("/docs/agents") },
});

// ---------------------------------------------------------------- llms.txt

const llms = `# Horde

> Horde is a local Rust daemon that turns one objective into a durable workflow and
> coordinates coding agents against a Git repository. Work is recorded in SQLite, so
> execution continues after the submitting client disconnects and interrupted runs
> resume instead of restarting.

Horde is free and open source (Apache-2.0), installs with one command on macOS and
Linux, and is driven either from its CLI (\`horde\`) or from any agent over the
Model Context Protocol. It is local-first: there is no hosted service, no account,
and no source code leaves the machines you run it on.

## When to use Horde

Reach for Horde when a coding task is too large or too long-running for a single
agent session and the work must survive a disconnect.

- **Multi-step repository work.** An objective needing planning, parallel
  implementation, and review, integrated onto one branch. Call \`submit_task\`
  with an objective and a repo path, then poll \`inspect\` or \`events\`.
- **Long-running jobs.** Work that outlives a terminal or chat session. State is
  durable; \`resume\` restarts interrupted work from where it stopped.
- **Coordinating several agents on one repository.** Workers claim exclusive file
  paths (\`claim_paths\`), exchange messages (\`send_message\`/\`read_messages\`), and
  share verified artifacts (\`put_artifact\`/\`reuse_artifact\`) instead of colliding.
- **Driving orchestration from your own agent.** Add the stdio MCP bridge and the
  full operation set becomes typed tool calls.

Do not use Horde for a single-shot edit to a file already open in your editor, or
as a hosted API — it runs on the user's own machine and must be installed first.

## How an agent calls it

1. Install: \`curl -fsSL https://horde.sh/install | bash\`
2. Start the daemon: \`horde start\`
3. Register the MCP server: \`{"mcpServers":{"horde":{"command":"horde","args":["mcp"]}}}\`
4. Or call operations directly: \`horde call submit_task '{"objective":"...","repo":"/path"}'\`

The bridge speaks MCP \`${catalog.transport.version}\` over stdio and answers \`tools/list\` with ${catalog.count} operations, of which ${catalog.tools.filter((t) => t.scope === "worker").length} are also available to restricted, task-scoped worker credentials.

## Machine-readable endpoints

- [MCP server manifest](${url("/.well-known/mcp.json")}): transport, install, platforms, and version. Start here.
- [Tool catalog](${url("/.well-known/tools.json")}): all ${catalog.count} operations with JSON Schema parameters, shaped as function-calling definitions.
- [Sitemap](${url("/sitemap.xml")}): every indexable URL.

The MCP discovery file uses Horde’s own metadata format, not an official MCP manifest schema.
It advertises only a local stdio transport; there is no remote MCP URL or HTTP handshake.

These are static descriptions of Horde, not a service. Horde runs on the user's
own machine and is driven over MCP or the CLI; there is no HTTP API to call.

Every page is also available as Markdown: send \`Accept: text/markdown\`, or append
\`.md\` to any path (${url("/docs.md")}).

## Documentation

${pages
  .filter((page) => page.path.startsWith("/docs"))
  .map((page) => `- [${page.title}](${url(page.path)}): ${page.summary}`)
  .join("\n")}

## About

${pages
  .filter((page) => !page.path.startsWith("/docs") && page.path !== "/")
  .map((page) => `- [${page.title}](${url(page.path)}): ${page.summary}`)
  .join("\n")}
- [Agent skills](https://github.com/gethamster/horde/tree/main/skills): three installable skills for the
  Agent Skills Protocol -- \`horde\` for driving Horde, \`horde-templates\` for authoring
  workflow templates, \`horde-worker\` for agents running inside a task. Install with
  \`npx skills add gethamster/horde\`.
- [Source code](https://github.com/gethamster/horde): public Apache-2.0 repository.
- [Release manifest](https://horde.sh/releases/latest/manifest.json): latest version, artifact URLs, and checksums.
- Issues: https://github.com/gethamster/horde/issues
- Contact: andrew@somervell.com

## Optional

- [Install script](${url("/install")}): redirects to the signed installer for the latest release.
- [Release signing key](${url("/release-key.pem")}): Ed25519 public key, fingerprint at /release-key.hex.
`;
generated.push(write("llms.txt", llms));

generated.push(
  write(
    "llms-full.txt",
    `${llms}\n---\n\n` +
      pages.map((page) => `${page.body}\n\n(Source: ${url(page.path)})`).join("\n\n---\n\n") +
      "\n",
  ),
);

// ---------------------------------------------------------------- sitemap

const sitemap =
  '<?xml version="1.0" encoding="UTF-8"?>\n' +
  '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n' +
  pages
    .map(
      (page) =>
        `  <url>\n    <loc>${url(page.path)}</loc>\n    <lastmod>${page.lastmod}</lastmod>\n` +
        `    <changefreq>${page.changefreq}</changefreq>\n    <priority>${page.priority}</priority>\n  </url>\n`,
    )
    .join("") +
  "</urlset>\n";
generated.push(write("sitemap.xml", sitemap));

generated.push(
  write(
    "robots.txt",
    `# horde.sh — all crawlers and agents welcome.
User-agent: *
Allow: /

Sitemap: ${url("/sitemap.xml")}

# Agent-facing summary and when-to-use guidance:
# ${url("/llms.txt")}
# MCP manifest: ${url("/.well-known/mcp.json")}
`,
  ),
);

// ------------------------------------------------------------ MCP manifest

json(".well-known/mcp.json", {
  // Horde-specific discovery metadata, not an official MCP manifest format.
  format: "horde-discovery",
  formatVersion: 1,
  name: "horde",
  displayName: "Horde",
  version,
  description:
    "Submit and steer durable coding tasks on a local Horde daemon: plan work into steps, run coordinated workers in isolated Git worktrees, and inspect durable events.",
  documentation: url("/docs/agents"),
  homepage: url("/"),
  repository: "https://github.com/gethamster/horde",
  skills: "https://github.com/gethamster/horde/tree/main/skills",
  releases: "https://horde.sh/releases/latest/manifest.json",
  license: "Apache-2.0",
  protocolVersion: catalog.transport.version,
  // Horde runs on the user's own machine. The server is the installed binary,
  // not a hosted endpoint, so there is no URL to connect to.
  transport: { type: "stdio", command: "horde", args: ["mcp"] },
  install: {
    instructions: url("/docs"),
    command: "curl -fsSL https://horde.sh/install | bash",
    platforms: ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64"],
  },
  capabilities: { tools: { listChanged: false } },
  toolCatalog: url("/.well-known/tools.json"),
  operations: {
    total: catalog.count,
    worker: catalog.tools.filter((tool) => tool.scope === "worker").length,
  },
  contact: {
    email: "andrew@somervell.com",
    issues: "https://github.com/gethamster/horde/issues",
  },
  tools: catalog.tools.map(({ name, description, scope }) => ({ name, description, scope })),
});

const listing = readdirSync(out, { recursive: true })
  .map(String)
  .filter((entry) => entry.endsWith(".md") || entry.endsWith(".json") || entry.endsWith(".txt"));
console.log(
  `Generated ${generated.length + 3} agent files for published Horde ${version}: ` +
    `${pages.length} Markdown variants, llms.txt, llms-full.txt, sitemap.xml, robots.txt, ` +
    `.well-known/mcp.json (${listing.length} machine-readable files in out/).`,
);
