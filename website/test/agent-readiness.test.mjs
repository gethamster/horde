// Verifies the built export actually carries the agent-facing surface.
//
// Run against `out/` after `npm run build`, so these assert what a crawler or
// agent would receive rather than what the source intends.
import { test, describe, before } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync, statSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { pages, documents, origin, url } from "../scripts/pages.mjs";
import { targets, serialize } from "../scripts/generate-vercel-config.mjs";
import { toMarkdown } from "../scripts/html-to-markdown.mjs";

const root = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const out = join(root, "out");
const read = (path) => readFileSync(join(out, path), "utf8");
const readJson = (path) => JSON.parse(read(path));

before(() => {
  assert.ok(
    existsSync(join(out, "index.html")),
    "out/ is missing; run `npm run build` before `npm test`",
  );
});

/** Visible text of a page, as a crawler that runs no JavaScript would see it. */
function visibleText(html) {
  return html
    .replace(/<script[\s\S]*?<\/script>/gi, "")
    .replace(/<style[\s\S]*?<\/style>/gi, "")
    .replace(/<[^>]+>/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

describe("content without JavaScript", () => {
  test("the homepage serves substantial content in raw HTML", () => {
    const text = visibleText(read("index.html"));
    assert.ok(text.length >= 500, `homepage has only ${text.length} characters of raw text`);
    assert.match(text, /durable workflow/i);
  });

  for (const page of pages) {
    test(`${page.path} has one h1 and sequential headings`, () => {
      const html = read(page.file);
      const h1 = html.match(/<h1\b/g) ?? [];
      assert.equal(h1.length, 1, `${page.path} has ${h1.length} h1 elements`);

      const levels = [...html.matchAll(/<h([1-6])\b/g)].map((match) => Number(match[1]));
      assert.equal(levels[0], 1, `${page.path} does not open with an h1`);
      for (let i = 1; i < levels.length; i += 1) {
        assert.ok(
          levels[i] <= levels[i - 1] + 1,
          `${page.path} jumps from h${levels[i - 1]} to h${levels[i]}`,
        );
      }
    });

    test(`${page.path} carries at least 500 characters of raw text`, () => {
      const text = visibleText(read(page.file));
      assert.ok(text.length >= 500, `${page.path} has only ${text.length} characters`);
    });
  }
});

describe("metadata completeness", () => {
  for (const page of pages) {
    test(`${page.path} declares lang, canonical, og:type and og:image`, () => {
      const html = read(page.file);
      assert.match(html, /<html[^>]*\blang="en"/, "missing html lang");
      const canonical = /<link[^>]*rel="canonical"[^>]*href="([^"]+)"/.exec(html);
      assert.ok(canonical, `${page.path} has no canonical link`);
      assert.equal(
        canonical[1].replace(/\/$/, ""),
        url(page.path).replace(/\/$/, ""),
        `${page.path} canonical points elsewhere`,
      );
      assert.match(html, /<meta property="og:type" content="website"/);
      const image = /<meta property="og:image" content="([^"]+)"/.exec(html);
      assert.ok(image, `${page.path} has no og:image`);
      assert.equal(image[1], url("/og.png"));
      assert.match(html, /<meta name="description" content="[^"]{50,}"/);
    });
  }

  test("the og:image is a real 1200x630 PNG", () => {
    const png = readFileSync(join(out, "og.png"));
    assert.equal(png.subarray(0, 8).toString("hex"), "89504e470d0a1a0a", "not a PNG");
    assert.equal(png.readUInt32BE(16), 1200, "unexpected width");
    assert.equal(png.readUInt32BE(20), 630, "unexpected height");
  });
});

describe("JSON-LD structured data", () => {
  const graph = () => {
    const match = /<script type="application\/ld\+json">([\s\S]*?)<\/script>/.exec(read("index.html"));
    assert.ok(match, "homepage has no JSON-LD block");
    return JSON.parse(match[1])["@graph"];
  };
  const node = (type) => graph().find((entry) => entry["@type"] === type);

  test("the homepage publishes a parseable graph", () => {
    assert.ok(Array.isArray(graph()) && graph().length >= 3);
  });

  test("SoftwareApplication carries name, description, url and offers", () => {
    const software = node("SoftwareApplication");
    assert.ok(software, "no SoftwareApplication node");
    for (const field of ["name", "description", "url", "applicationCategory", "operatingSystem"]) {
      assert.ok(software[field], `SoftwareApplication is missing ${field}`);
    }
    assert.equal(software.offers["@type"], "Offer");
    assert.equal(software.offers.price, "0");
  });

  test("Organization carries a contactPoint with an email", () => {
    const organization = node("Organization");
    assert.ok(organization, "no Organization node");
    assert.ok(organization.email, "Organization has no email");
    const contacts = organization.contactPoint;
    assert.ok(Array.isArray(contacts) && contacts.length > 0, "no contactPoint");
    for (const contact of contacts) {
      assert.equal(contact["@type"], "ContactPoint");
      assert.ok(contact.contactType, "contactPoint has no contactType");
      assert.ok(contact.email || contact.url, "contactPoint has no email or url");
    }
    assert.ok(Array.isArray(organization.sameAs) && organization.sameAs.length > 0);
  });
});

describe("agent-friendly 404", () => {
  test("the 404 page points at the recovery routes", () => {
    const html = read("404.html");
    const text = visibleText(html);
    assert.ok(text.length >= 500, `404 body has only ${text.length} characters`);
    for (const path of ["/llms.txt", "/sitemap.xml", "/.well-known/tools.json", "/docs"]) {
      assert.ok(html.includes(path), `404 page does not link ${path}`);
    }
    assert.match(html, /<meta name="robots" content="noindex/);
  });

  test("a Markdown 404 body is published", () => {
    const markdown = read("404.md");
    assert.match(markdown, /^# 404/);
    assert.ok(markdown.includes(url("/llms.txt")), "404.md does not link llms.txt");
    assert.ok(markdown.includes(url("/sitemap.xml")), "404.md does not link the sitemap");
  });
});

describe("markdown content negotiation", () => {
  for (const page of pages) {
    test(`${page.path} has a Markdown variant`, () => {
      const markdown = read(page.markdown);
      assert.ok(markdown.length >= 300, `${page.markdown} is only ${markdown.length} characters`);
      assert.match(markdown, /^# \S/, `${page.markdown} does not open with an h1`);
      assert.ok(markdown.includes(url(page.path)), `${page.markdown} does not cite its source URL`);
      assert.ok(!/<[a-z][^>]*>/i.test(markdown), `${page.markdown} still contains HTML tags`);
      assert.ok(
        !/\]\(\/(?!\/)/.test(markdown),
        `${page.markdown} contains site-relative links that will not resolve off-site`,
      );
    });

    test(`${page.path} Markdown tracks the rendered page`, () => {
      // The variant is derived from the HTML, so headings must line up exactly.
      const fromHtml = toMarkdown(read(page.file), origin)
        .split("\n")
        .filter((line) => line.startsWith("## "));
      const fromMarkdown = read(page.markdown)
        .split("\n")
        .filter((line) => line.startsWith("## "));
      assert.deepEqual(fromMarkdown, fromHtml, `${page.markdown} is out of step with the page`);
    });
  }

  test("every page is routed by Accept and declares Vary", () => {
    for (const target of targets) {
      const { routes } = target.config;
      for (const page of pages) {
        const rule = routes.find((entry) => entry.src && new RegExp(entry.src).test(page.path) && entry.status === 307 && entry.has);
        assert.ok(rule, `${page.path} has no Accept redirect`);
        assert.equal(rule.headers.Location, `/${page.markdown}`);
        assert.match(rule.has[0].value, /text\/markdown/);
        const header = routes.find((entry) => entry.continue && new RegExp(entry.src).test(page.path) && entry.headers.Vary);
        assert.ok(header, `${page.path} has no Vary header rule`);
        assert.match(header.headers.Vary, /\bAccept\b/);
      }
    }
  });

  test("the committed vercel.json files match the generator", () => {
    for (const target of targets) {
      assert.equal(
        readFileSync(target.path, "utf8"),
        serialize(target.config),
        `${target.path.pathname} is stale; run node scripts/generate-vercel-config.mjs`,
      );
    }
  });
});

describe("function calling compatibility", () => {
  const catalog = () => readJson(".well-known/tools.json");

  test("every tool has a unique id, a description and a closed typed schema", () => {
    const { tools, count } = catalog();
    assert.equal(tools.length, count);
    const names = new Set();
    for (const tool of tools) {
      assert.ok(/^[a-z][a-z0-9_]*$/.test(tool.name), `${tool.name} is not a valid function name`);
      assert.ok(!names.has(tool.name), `duplicate operation id ${tool.name}`);
      names.add(tool.name);
      assert.ok(tool.description?.length > 10, `${tool.name} has no usable description`);
      assert.ok(["admin", "worker"].includes(tool.scope), `${tool.name} has no scope`);
      assert.equal(tool.parameters.type, "object");
      assert.equal(tool.parameters.additionalProperties, false, `${tool.name} has an open schema`);
      for (const [key, schema] of Object.entries(tool.parameters.properties)) {
        assert.ok(
          ["string", "integer", "number", "boolean", "array", "object"].includes(schema.type),
          `${tool.name}.${key} has type ${schema.type}`,
        );
        if (schema.type === "array") assert.ok(schema.items, `${tool.name}.${key} has untyped items`);
      }
      for (const required of tool.parameters.required) {
        assert.ok(tool.parameters.properties[required], `${tool.name} requires unknown ${required}`);
      }
    }
  });

  test("the catalog names its transport", () => {
    const { transport } = catalog();
    assert.equal(transport.protocol, "mcp");
    assert.equal(transport.kind, "stdio");
    assert.ok(transport.version && transport.command);
  });
});

describe("MCP manifest", () => {
  test("is published with transport, install and tools", () => {
    const manifest = readJson(".well-known/mcp.json");
    assert.equal(manifest.name, "horde");
    assert.ok(manifest.version && manifest.description && manifest.protocolVersion);
    assert.equal(manifest.transport.type, "stdio");
    assert.ok(manifest.install.command.includes("horde.sh/install"));
    assert.ok(manifest.install.platforms.length >= 2);
    assert.equal(manifest.tools.length, readJson(".well-known/tools.json").count);
    assert.equal(manifest.toolCatalog, url("/.well-known/tools.json"));
  });
});

describe("llms.txt", () => {
  const llms = () => read("llms.txt");

  test("opens with the product name and a blockquote summary", () => {
    assert.match(llms(), /^# Horde\n\n> /);
  });

  test("tells an agent when to use Horde and when not to", () => {
    const text = llms();
    assert.match(text, /^## When to use Horde$/m, "no when-to-use section");
    const section = text.split("## When to use Horde")[1].split("\n## ")[0];
    assert.ok(section.length >= 400, "when-to-use guidance is too thin");
    assert.ok(section.includes("submit_task"), "guidance names no concrete operation");
    assert.match(text, /Do not use Horde/, "no negative guidance");
  });

  test("links every machine-readable endpoint and page", () => {
    const text = llms();
    for (const path of ["/.well-known/tools.json", "/.well-known/mcp.json", "/sitemap.xml"]) {
      assert.ok(text.includes(url(path)), `llms.txt does not link ${path}`);
    }
    for (const page of pages.filter((entry) => entry.path !== "/")) {
      assert.ok(text.includes(url(page.path)), `llms.txt does not link ${page.path}`);
    }
  });

  test("llms-full.txt inlines every page", () => {
    const full = read("llms-full.txt");
    for (const page of pages) {
      assert.ok(full.includes(url(page.path)), `llms-full.txt omits ${page.path}`);
    }
    assert.ok(full.length > llms().length);
  });
});

describe("sitemap and robots", () => {
  test("the sitemap lists every page with a lastmod", () => {
    const xml = read("sitemap.xml");
    assert.match(xml, /^<\?xml version="1\.0" encoding="UTF-8"\?>/);
    assert.match(xml, /<urlset xmlns="http:\/\/www\.sitemaps\.org\/schemas\/sitemap\/0\.9">/);
    const locations = [...xml.matchAll(/<loc>([^<]+)<\/loc>/g)].map((match) => match[1]);
    assert.deepEqual(locations.sort(), pages.map((page) => url(page.path)).sort());
    for (const lastmod of [...xml.matchAll(/<lastmod>([^<]+)<\/lastmod>/g)].map((m) => m[1])) {
      assert.ok(!Number.isNaN(Date.parse(lastmod)), `invalid lastmod ${lastmod}`);
    }
    assert.ok(statSync(join(out, "sitemap.xml")).size < 50 * 1024 * 1024);
  });

  test("robots.txt allows crawling and names the sitemap", () => {
    const text = read("robots.txt");
    assert.match(text, /^User-agent: \*$/m);
    assert.match(text, /^Allow: \/$/m);
    assert.ok(text.includes(`Sitemap: ${url("/sitemap.xml")}`));
  });
});

describe("published documents", () => {
  for (const path of documents) {
    test(`${path} exists and is well formed`, () => {
      const file = path.replace(/^\//, "");
      assert.ok(existsSync(join(out, file)), `${path} was not published`);
      if (path.endsWith(".json")) assert.doesNotThrow(() => readJson(file));
      assert.ok(statSync(join(out, file)).size > 0, `${path} is empty`);
    });

    test(`${path} is served with CORS`, () => {
      for (const target of targets) {
        const rule = target.config.routes.find((entry) => entry.continue && new RegExp(entry.src).test(path) && entry.headers["Access-Control-Allow-Origin"]);
        assert.ok(rule, `${path} has no header rule`);
        const cors = rule.headers["Access-Control-Allow-Origin"];
        assert.equal(cors, "*", `${path} is not readable cross-origin`);
      }
    });
  }

  test("the manifest agrees with the catalog", () => {
    const manifest = readJson(".well-known/mcp.json");
    const catalog = readJson(".well-known/tools.json");
    assert.equal(manifest.operations.total, catalog.count);
    assert.equal(
      manifest.operations.worker,
      catalog.tools.filter((tool) => tool.scope === "worker").length,
    );
    assert.ok(manifest.contact.email && manifest.license);
    assert.equal(manifest.protocolVersion, catalog.transport.version);
  });
});

describe("published links stay public", () => {
  // Source and skills share the public repository; downloads use stable Horde URLs.
  const publicRepositories = ["gethamster/horde"];
  const files = readdirSync(out, { recursive: true })
    .map(String)
    .filter((entry) => /\.(html|md|txt|json|xml)$/.test(entry));

  test("no published file links the private source repository", () => {
    const offenders = [];
    for (const file of files) {
      for (const match of read(file).matchAll(/github\.com\/(?:asomervell|gethamster)\/[A-Za-z0-9_.-]+/g)) {
        const repository = match[0].replace("github.com/", "");
        if (!publicRepositories.includes(repository)) offenders.push(`${file} -> ${match[0]}`);
      }
    }
    assert.deepEqual(offenders, [], `these link a non-public repository:\n${offenders.join("\n")}`);
  });

  test("the public repositories are actually linked", () => {
    const everything = files.map((file) => read(file)).join("\n");
    for (const repository of publicRepositories) {
      assert.ok(everything.includes(repository), `nothing links ${repository}`);
    }
  });

  test("the manifest points at a reachable issue tracker", () => {
    const manifest = readJson(".well-known/mcp.json");
    assert.ok(
      manifest.contact.issues === "https://github.com/gethamster/horde/issues",
      `the manifest points issues at ${manifest.contact.issues}`,
    );
    assert.equal(manifest.skills, "https://github.com/gethamster/horde/tree/main/skills");
    assert.equal(manifest.repository, "https://github.com/gethamster/horde");
  });

  test("the organization links the public source repository", () => {
    const match = /<script type="application\/ld\+json">([\s\S]*?)<\/script>/.exec(read("index.html"));
    const graph = JSON.parse(match[1])["@graph"];
    const software = graph.find((node) => node["@type"] === "SoftwareApplication");
    assert.ok(!software.codeRepository, "SoftwareApplication advertises a private codeRepository");
    const organization = graph.find((node) => node["@type"] === "Organization");
    for (const url of organization.sameAs) {
      assert.ok(
        publicRepositories.some((repository) => url.includes(repository)),
        `sameAs points at ${url}`,
      );
    }
  });
});

describe("discoverability", () => {
  test("every page title names the product", () => {
    for (const page of pages) {
      const title = /<title>([^<]*)<\/title>/.exec(read(page.file))?.[1] ?? "";
      assert.match(title, /Horde/i, `${page.path} title "${title}" does not name Horde`);
    }
  });

  test("the trust anchor pages are substantial", () => {
    for (const path of ["/about", "/contact", "/privacy"]) {
      const page = pages.find((entry) => entry.path === path);
      const text = visibleText(read(page.file));
      assert.ok(text.length >= 500, `${path} has only ${text.length} characters`);
    }
  });

  test("the site links its developer resources from the homepage", () => {
    const html = read("index.html");
    for (const path of ["/docs", "/docs/agents", "/llms.txt", "/.well-known/tools.json"]) {
      assert.ok(html.includes(path), `homepage does not link ${path}`);
    }
  });
});


describe("honest agent discovery", () => {
  test("discovery uses a versioned Horde format and only local stdio", () => {
    const manifest = readJson(".well-known/mcp.json");
    assert.equal(manifest.$schema, undefined);
    assert.equal(manifest.format, "horde-discovery");
    assert.equal(manifest.formatVersion, 1);
    assert.deepEqual(manifest.transport, { type: "stdio", command: "horde", args: ["mcp"] });
    assert.match(read("docs/agents.md"), /no HTTP API, OpenAPI specification, or Streamable HTTP MCP endpoint/);
    assert.match(read("llms.txt"), /no remote MCP URL or HTTP handshake/);
    assert.ok(!existsSync(join(out, "openapi.json")));
  });
  test("developer reference and discovery links identify Horde", () => {
    const html = read("docs/agents.html");
    assert.match(html, /<h1>Horde MCP and CLI reference<\/h1>/);
    assert.match(html, /<title>MCP and CLI reference · Horde docs<\/title>/);
    for (const page of pages) {
      const html = read(page.file);
      assert.match(html, /rel="help"[^>]*href="https:\/\/www.horde.sh\/docs\/agents"/);
      assert.match(html, /href="https:\/\/www.horde.sh\/llms.txt"[^>]*title="Horde agent index"/);
    }
  });
});

test("public business address is consistent in Organization data and contact documents", () => {
  const expected = {
    "@type": "PostalAddress", streetAddress: "425 2nd St STE 500",
    addressLocality: "San Francisco", addressRegion: "CA", postalCode: "94107", addressCountry: "US",
  };
  for (const page of pages) {
    const graph = JSON.parse(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/.exec(read(page.file))[1])["@graph"];
    assert.deepEqual(graph.find((node) => node["@type"] === "Organization").address, expected);
  }
  for (const path of ["contact.html", "contact.md", "llms-full.txt"]) {
    const body = path.endsWith(".html") ? visibleText(read(path)) : read(path);
    assert.ok(body.includes(expected.streetAddress), path);
    assert.ok(body.includes("San Francisco, CA 94107"), path);
  }
});

test("published documentation uses organization skills links and stable download URLs", () => {
  const manifest = readJson(".well-known/mcp.json");
  assert.equal(manifest.skills, "https://github.com/gethamster/horde/tree/main/skills");
  assert.equal(manifest.releases, "https://horde.sh/releases/latest/manifest.json");
  for (const page of pages) {
    for (const path of [page.file, page.markdown]) assert.ok(!read(path).includes("asomervell"), path);
  }
  for (const path of ["llms.txt", "llms-full.txt", ".well-known/mcp.json"]) {
    assert.ok(!read(path).includes("asomervell"), path);
    assert.ok(read(path).includes("gethamster/horde"), path);
  }
  assert.ok(read("llms.txt").includes("npx skills add gethamster/horde"));
});

test("the main repository owns exactly the three discoverable skills", () => {
  const skillsRoot = join(root, "..", "skills");
  const names = readdirSync(skillsRoot).filter((name) => existsSync(join(skillsRoot, name, "SKILL.md"))).sort();
  assert.deepEqual(names, ["horde", "horde-templates", "horde-worker"]);
  const grouping = JSON.parse(readFileSync(join(root, "..", "skills.sh.json"), "utf8"));
  assert.deepEqual(grouping.groupings.flatMap((group) => group.skills).sort(), names);
  const readme = readFileSync(join(root, "..", "README.md"), "utf8");
  assert.ok(readme.includes("npx skills add gethamster/horde"));
  assert.ok(readme.includes("https://skills.sh/gethamster/horde"));
  assert.ok(!existsSync(join(root, "..", "scripts", "sync_skills.sh")));
});


test("README documentation entry links to a complete guide index", () => {
  const repository = join(root, "..");
  const readme = readFileSync(join(repository, "README.md"), "utf8");
  assert.ok(readme.includes("[Documentation](docs/README.md)"));
  const docs = join(repository, "docs");
  const index = readFileSync(join(docs, "README.md"), "utf8");
  for (const name of readdirSync(docs).filter((name) => name.endsWith(".md") && name !== "README.md")) {
    assert.ok(index.includes(`](${name})`), `missing guide: ${name}`);
  }
});
