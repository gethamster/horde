// HTTP checks of the built export using the committed Vercel route table.
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFileSync, existsSync, statSync } from "node:fs";
import { join, extname } from "node:path";
import { website } from "../scripts/generate-vercel-config.mjs";
import { pages, documents, retired } from "../scripts/pages.mjs";

const out = new URL("../out/", import.meta.url).pathname;

const types = {
  ".html": "text/html; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".xml": "application/xml; charset=utf-8",
  ".md": "text/markdown; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".pem": "application/x-pem-file",
};

const resolve = (path) => {
  const file = join(out, path === "/" ? "index.html" : path);
  return existsSync(file) && statSync(file).isFile() ? file : null;
};
const conditionsMet = (rule, request) =>
  (rule.has ?? []).every((condition) =>
    new RegExp(`^${condition.value}$`).test(request.headers[condition.key] ?? ""),
  );

const server = createServer((request, response) => {
  const { pathname } = new URL(request.url, "http://localhost");
  const headers = {};
  const send = (file, status = 200) => {
    response.writeHead(status, { "Content-Type": types[extname(file)] ?? "application/octet-stream", ...headers });
    response.end(request.method === "HEAD" ? undefined : readFileSync(file));
  };
  for (const route of website.routes) {
    if (route.handle === "filesystem") {
      const file = resolve(pathname);
      if (file) return send(file);
      continue;
    }
    const match = new RegExp(route.src).exec(pathname);
    if (!match || !conditionsMet(route, request)) continue;
    const substitute = (value) => value.replace(/\$(\d+)/g, (_, n) => match[Number(n)] ?? "");
    for (const [key, value] of Object.entries(route.headers ?? {})) headers[key] = substitute(value);
    if (route.continue) continue;
    if (route.status >= 300 && route.status < 400) {
      response.writeHead(route.status, headers);
      response.end();
      return;
    }
    if (route.dest) {
      const file = resolve(substitute(route.dest));
      if (file) return send(file, route.status);
    }
  }
  response.writeHead(500);
  response.end("No terminal route matched");
});

let base;
before(async () => {
  assert.ok(existsSync(join(out, "index.html")), "run `npm run build` before `npm test`");
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  base = `http://127.0.0.1:${server.address().port}`;
});
after(() => server.close());

const get = (path, headers = {}) => fetch(new URL(path, base), { headers, redirect: "manual" });
const follow = (path, headers = {}) => fetch(new URL(path, base), { headers, redirect: "follow" });

describe("HTML responses", () => {
  for (const page of pages) {
    test(`GET ${page.path} serves HTML`, async () => {
      const response = await get(page.path);
      assert.equal(response.status, 200);
      assert.match(response.headers.get("content-type"), /text\/html/);
      assert.match(response.headers.get("vary") ?? "", /\bAccept\b/);
      const body = await response.text();
      assert.match(body, /<h1[\s>]/);
    });
  }
});

describe("markdown content negotiation", () => {
  for (const page of pages) {
    test(`GET ${page.path} with Accept: text/markdown negotiates to Markdown`, async () => {
      // Vercel cannot serve a different body at the same URL from a static
      // export, so negotiation redirects to the .md variant. The redirect
      // itself must vary on Accept or a cache will pin one branch for both.
      const hop = await get(page.path, { accept: "text/markdown" });
      assert.ok(
        [301, 302, 307, 308].includes(hop.status),
        `${page.path} returned ${hop.status} instead of redirecting to Markdown`,
      );
      assert.equal(hop.headers.get("location"), `/${page.markdown}`);
      assert.match(
        hop.headers.get("vary") ?? "",
        /\bAccept\b/,
        "a negotiated redirect must vary on Accept",
      );

      const response = await follow(page.path, { accept: "text/markdown" });
      assert.equal(response.status, 200);
      assert.match(response.headers.get("content-type"), /text\/markdown/);
      const body = await response.text();
      assert.match(body, /^# \S/, "Markdown variant does not open with a heading");
      assert.ok(!body.includes("<html"), "HTML was served to a Markdown request");
    });

    test(`GET ${page.path} with a browser Accept still serves HTML`, async () => {
      const response = await get(page.path, {
        accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
      });
      assert.match(response.headers.get("content-type"), /text\/html/);
    });

    test(`GET /${page.markdown} serves Markdown directly`, async () => {
      const response = await get(`/${page.markdown}`);
      assert.equal(response.status, 200);
      assert.match(response.headers.get("content-type"), /text\/markdown/);
    });
  }

  test("a quality-listed Accept header still negotiates", async () => {
    const response = await follow("/docs", { accept: "text/markdown;q=1.0, text/html;q=0.8" });
    assert.match(response.headers.get("content-type"), /text\/markdown/);
  });

  test("negotiation runs before the filesystem and error fallbacks run after it", () => {
    const filesystem = website.routes.findIndex((route) => route.handle === "filesystem");
    assert.ok(filesystem > 0);
    assert.ok(website.routes.slice(0, filesystem).some((route) => route.status === 307 && route.has));
    assert.ok(website.routes.slice(filesystem + 1).every((route) => route.status === 404));
  });
});

describe("machine-readable documents", () => {
  for (const path of documents) {
    test(`GET ${path} is reachable and typed`, async () => {
      const response = await get(path);
      assert.equal(response.status, 200, `${path} is not reachable`);
      assert.equal(response.headers.get("access-control-allow-origin"), "*");
      assert.equal(response.headers.get("x-content-type-options"), "nosniff");
      const body = await response.text();
      assert.ok(body.length > 0, `${path} is empty`);
      if (path.endsWith(".json")) {
        assert.match(response.headers.get("content-type"), /application\/json/);
        assert.doesNotThrow(() => JSON.parse(body), `${path} is not valid JSON`);
      }
    });
  }

  test("GET /og.png serves a PNG", async () => {
    const response = await get("/og.png");
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("content-type"), "image/png");
  });
});

describe("retired paths", () => {
  // These were published under the old API framing. They must not 404.
  for (const entry of retired) {
    test(`GET ${entry.from} redirects to ${entry.to}`, async () => {
      const hop = await get(entry.from);
      assert.ok(
        [301, 308].includes(hop.status),
        `${entry.from} returned ${hop.status} instead of a permanent redirect`,
      );
      assert.equal(hop.headers.get("location"), entry.to);

      const final = await follow(entry.from);
      assert.equal(final.status, 200, `${entry.from} redirects to something that does not exist`);
    });
  }

  test("nothing published still advertises a retired path", () => {
    const retiredPaths = retired.map((entry) => entry.from);
    const offenders = [];
    for (const page of pages) {
      const body = readFileSync(join(out, page.file), "utf8");
      for (const path of retiredPaths) {
        if (body.includes(`"${path}"`)) offenders.push(`${page.path} -> ${path}`);
      }
    }
    assert.deepEqual(offenders, [], `still linking retired paths:\n${offenders.join("\n")}`);
  });
});

describe("404 handling", () => {
  for (const path of ["/does-not-exist", "/docs/does-not-exist", "/well-known/nope.json"]) {
    test(`GET ${path} returns 404 with a recovery body`, async () => {
      const response = await get(path);
      assert.equal(response.status, 404, `${path} must not resolve`);
      const body = await response.text();
      for (const link of ["/llms.txt", "/sitemap.xml", "/docs"]) {
        assert.ok(body.includes(link), `the 404 body does not point at ${link}`);
      }
    });
  }
});

for (const path of ["/missing", "/docs/missing", "/openapi.json", "/api/openapi.yaml", "/mcp"]) {
  for (const [accept, type] of [["*/*", "text/markdown"], ["text/markdown", "text/markdown"], ["text/html", "text/html"], ["application/json", "application/problem+json"], ["application/problem+json", "application/problem+json"]]) {
    test(`${path} returns a typed 404 for ${accept}`, async () => {
      const response = await get(path, { accept });
      assert.equal(response.status, 404);
      assert.equal(response.headers.get("location"), null);
      assert.equal(response.headers.get("content-type"), `${type}; charset=utf-8`);
      assert.match(response.headers.get("vary"), /Accept/);
      assert.equal(response.headers.get("x-robots-tag"), "noindex");
      const body = await response.text();
      if (type === "text/markdown") assert.match(body, /^# 404/);
      if (type === "application/problem+json") {
        const problem = JSON.parse(body);
        assert.equal(problem.type, "about:blank");
        assert.equal(problem.status, response.status);
        assert.equal(problem.code, "not_found");
        assert.ok(problem.detail && problem.hint);
        for (const target of Object.values(problem.links)) {
          assert.equal((await get(new URL(target).pathname)).status, 200);
        }
      }
    });
  }
}
test("HEAD on a missing path preserves the 404 headers without a body", async () => {
  const response = await fetch(new URL("/missing", base), { method: "HEAD", headers: { accept: "text/markdown" } });
  assert.equal(response.status, 404);
  assert.match(response.headers.get("content-type"), /text\/markdown/);
  assert.equal(await response.text(), "");
});
for (const page of pages) {
  test(`clean URL redirect for ${page.file}`, async () => {
    const response = await get(`/${page.file}`);
    assert.equal(response.status, 308);
    assert.equal(response.headers.get("location"), page.path);
  });
}

for (const [path, destination] of [
  ["/install", "https://github.com/asomervell/horde-releases/releases/latest/download/install.sh"],
  ["/releases/latest/manifest.json", "https://github.com/asomervell/horde-releases/releases/latest/download/manifest.json"],
  ["/releases/v0.4.0/checksums.txt", "https://github.com/asomervell/horde-releases/releases/download/v0.4.0/checksums.txt"],
]) {
  test(`${path} retains its release redirect without an external write`, async () => {
    const response = await get(path);
    assert.equal(response.status, 307);
    assert.equal(response.headers.get("location"), destination);
    assert.equal(response.headers.get("cache-control"), "no-store");
  });
}
for (const path of ["/release-key.pem", "/release-key.hex", "/icon.svg", "/404.md", "/404.json"]) {
  test(`published asset ${path} is reachable`, async () => {
    const response = await get(path);
    assert.equal(response.status, 200);
    const body = await response.text();
    assert.ok(body.length > 0);
    if (path.endsWith(".json")) assert.doesNotThrow(() => JSON.parse(body));
  });
}
test("every built stylesheet and script survives fallback routing", async () => {
  const html = readFileSync(join(out, "index.html"), "utf8");
  const assets = new Set([...html.matchAll(/(?:src|href)="(\/_next\/[^"?]+)[^"]*"/g)].map((match) => match[1]));
  assert.ok(assets.size > 0);
  for (const asset of assets) assert.equal((await get(asset)).status, 200, asset);
});

for (const page of pages.filter((page) => page.path !== "/")) {
  test(`${page.path}/ normalizes before Markdown negotiation`, async () => {
    const hop = await get(`${page.path}/`, { accept: "text/markdown" });
    assert.equal(hop.status, 308);
    assert.equal(hop.headers.get("location"), page.path);
    const response = await follow(`${page.path}/`, { accept: "text/markdown" });
    assert.equal(response.status, 200);
    assert.match(response.headers.get("content-type"), /text\/markdown/);
    assert.match(response.headers.get("vary"), /Accept/);
  });
}
