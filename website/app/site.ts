// Single source of truth for absolute URLs and identity used by page metadata,
// JSON-LD, and the generated sitemap, llms.txt, and discovery metadata.
//
// `origin` must be the host that actually serves the site: the apex currently
// redirects to www, and a canonical URL that redirects is worse than no
// canonical at all. Flip this constant if the Vercel primary domain changes.
export const origin = "https://www.horde.sh";

export const site = {
  origin,
  name: "Horde",
  domain: "horde.sh",
  tagline: "Durable task orchestration for coding agents",
  description:
    "Horde is a local Rust daemon that turns a task into a durable workflow and coordinates coding agents. Submit work from your terminal or connect your own agent over MCP; execution continues after the client disconnects.",
  // Source and agent skills share one public repository.
  repository: "https://github.com/gethamster/horde",
  skills: "https://github.com/gethamster/horde/tree/main/skills",
  releases: "https://horde.sh/releases/latest/manifest.json",
  issues: "https://github.com/gethamster/horde/issues",
  skillsInstall: "npx skills add gethamster/horde",
  email: "andrew@somervell.com",
  author: "Andrew Somervell",
  address: {
    "@type": "PostalAddress",
    streetAddress: "425 2nd St STE 500",
    addressLocality: "San Francisco",
    addressRegion: "CA",
    postalCode: "94107",
    addressCountry: "US",
  },
  license: "Apache-2.0",
  licenseUrl: "https://www.apache.org/licenses/LICENSE-2.0",
  install: "curl -fsSL https://horde.sh/install | bash",
} as const;

export const url = (path: string) => new URL(path, origin).toString();

// The indexable page list lives in scripts/pages.mjs: it drives the sitemap,
// llms.txt, the Markdown variants, and the vercel.json routing.
