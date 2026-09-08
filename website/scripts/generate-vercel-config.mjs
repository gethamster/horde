// Generates both vercel.json files from the shared page list.
//
// Vercel reads vercel.json before the build runs, so these must be committed.
// `npm test` asserts the committed files match this generator, which keeps the
// Accept-header rewrites and Vary headers in step with the pages.
import { writeFileSync, readFileSync } from "node:fs";
import { getTransformedRoutes, normalizeRoutes } from "@vercel/routing-utils";
import { pages, documents, retired } from "./pages.mjs";

const markdownAccept = { type: "header", key: "accept", value: "(.*text/markdown.*)" };

// A negotiated response must name Accept in Vary, or a CDN can hand the cached
// HTML variant to an agent that asked for Markdown.
const vary = { key: "Vary", value: "Accept, Accept-Encoding" };
const cors = { key: "Access-Control-Allow-Origin", value: "*" };
const noSniff = { key: "X-Content-Type-Options", value: "nosniff" };

const releaseRedirects = [
  {
    source: "/install",
    destination: "https://github.com/asomervell/horde-releases/releases/latest/download/install.sh",
    permanent: false,
  },
  {
    source: "/releases/latest/:file",
    destination: "https://github.com/asomervell/horde-releases/releases/latest/download/:file",
    permanent: false,
  },
  {
    source: "/releases/:version/:file",
    destination: "https://github.com/asomervell/horde-releases/releases/download/:version/:file",
    permanent: false,
  },
];

const routing = {
  $schema: "https://openapi.vercel.sh/vercel.json",
  framework: null,

  // Send an agent asking for Markdown to the Markdown variant.
  //
  // This has to be a redirect, not a rewrite. Vercel evaluates `rewrites` only
  // after the filesystem check, so a rewrite on /docs never runs: docs.html
  // already satisfies the request. `redirects` are evaluated before it, which
  // is the only pre-filesystem hook a static export gets.
  redirects: [
    ...pages.map((page) => ({
      source: page.path,
      has: [markdownAccept],
      destination: `/${page.markdown}`,
      // The target varies by Accept, so this must not be cached as permanent.
      permanent: false,
    })),
    // Paths that were live under the old API framing.
    ...retired.map((entry) => ({ source: entry.from, destination: entry.to, permanent: true })),
    ...releaseRedirects,
  ],

  headers: [
    // Negotiated page responses. Vary must be on both the HTML and the redirect
    // to the Markdown variant, or a cache can serve one to a request that asked
    // for the other.
    ...pages.map((page) => ({ source: page.path, headers: [vary] })),
    // Markdown variants, fetched directly rather than negotiated. Listed
    // explicitly: Vercel matches literal paths far more predictably than a
    // wildcard with a suffix.
    ...[...pages.map((page) => `/${page.markdown}`), "/404.md"].map((path) => ({
      source: path,
      headers: [
        { key: "Content-Type", value: "text/markdown; charset=utf-8" },
        vary,
        cors,
        noSniff,
      ],
    })),
    // Machine-readable documents are fetched cross-origin by agents.
    ...documents.map((path) => ({
      source: path,
      headers: [
        cors,
        noSniff,
        { key: "Cache-Control", value: "public, max-age=300, must-revalidate" },
      ],
    })),
    { source: "/install", headers: [{ key: "Cache-Control", value: "no-store" }] },
    {
      source: "/release-key.:extension",
      headers: [
        { key: "Cache-Control", value: "public, max-age=3600" },
        noSniff,
      ],
    },
    { source: "/releases/:path*", headers: [{ key: "Cache-Control", value: "no-store" }] },
  ],

  cleanUrls: true,
};

// Low-level routes allow a fallback body AND a real 404 status. Use Vercel’s
// own compiler for existing headers, redirects and clean-URL normalization.
const compile = (input) => {
  const result = getTransformedRoutes(input);
  if (result.error) throw new Error(JSON.stringify(result.error));
  return result.routes ?? [];
};
const errorHeaders = {
  "Vary": "Accept, Accept-Encoding",
  "Access-Control-Allow-Origin": "*",
  "X-Content-Type-Options": "nosniff",
  "X-Robots-Tag": "noindex",
  "Cache-Control": "public, max-age=0, must-revalidate",
};
const fallback = (dest, type, has) => ({
  src: "/.*", dest, status: 404,
  ...(has ? { has: [{ type: "header", key: "accept", value: has }] } : {}),
  headers: { ...errorHeaders, "Content-Type": `${type}; charset=utf-8` },
});
const routes = [
  ...compile({ headers: routing.headers }),
  ...compile({ cleanUrls: true, trailingSlash: false, redirects: routing.redirects }),
  // Explicit page destinations retain clean URLs without a catch-all app shell.
  ...pages.map((page) => ({
    src: `^${page.path}$`, dest: `/${page.file}`,
  })),
  { handle: "filesystem" },
  fallback("/404.json", "application/problem+json", ".*application/(?:problem\\+)?json.*"),
  fallback("/404.md", "text/markdown", ".*text/markdown.*"),
  fallback("/404.html", "text/html", ".*text/html.*"),
  fallback("/404.md", "text/markdown"),
];
const validated = normalizeRoutes(routes);
if (validated.error) throw new Error(JSON.stringify(validated.error));
const config = (deployment) => ({
  $schema: routing.$schema, framework: null, ...deployment, routes: validated.routes,
});

export const website = config({
  installCommand: "npm ci",
  buildCommand: "npm run build",
  outputDirectory: "out",
});

export const repositoryRoot = config({
  installCommand: "npm ci --prefix website",
  buildCommand: "npm run build --prefix website",
  outputDirectory: "website/out",
});

export const serialize = (value) => JSON.stringify(value, null, 2) + "\n";

export const targets = [
  { path: new URL("../vercel.json", import.meta.url), config: website },
  { path: new URL("../../vercel.json", import.meta.url), config: repositoryRoot },
];

if (import.meta.url === `file://${process.argv[1]}`) {
  for (const target of targets) {
    const body = serialize(target.config);
    const current = (() => {
      try {
        return readFileSync(target.path, "utf8");
      } catch {
        return "";
      }
    })();
    writeFileSync(target.path, body);
    console.log(`${current === body ? "unchanged" : "updated"} ${target.path.pathname}`);
  }
}
