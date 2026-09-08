import type { Metadata } from "next";
import { origin, site, url } from "./site";
import "./globals.css";

export const metadata: Metadata = {
  metadataBase: new URL(origin),
  title: { default: `${site.name} — ${site.tagline}`, template: `%s · ${site.name}` },
  description: site.description,
  applicationName: site.name,
  authors: [{ name: site.author }],
  keywords: [
    "Horde",
    "horde.sh",
    "coding agents",
    "agent orchestration",
    "MCP server",
    "Model Context Protocol",
    "durable workflow",
    "developer CLI",
  ],
  alternates: { canonical: url("/") },
  openGraph: {
    type: "website",
    siteName: site.name,
    title: `${site.name} — ${site.tagline}`,
    description: site.description,
    url: url("/"),
    locale: "en_US",
    images: [{ url: url("/og.png"), width: 1200, height: 630, alt: `${site.name} — ${site.tagline}` }],
  },
  twitter: {
    card: "summary_large_image",
    title: `${site.name} — ${site.tagline}`,
    description: site.description,
    images: [url("/og.png")],
  },
  robots: { index: true, follow: true },
  icons: { icon: [{ url: "/icon.svg", type: "image/svg+xml" }] },
};

// Identity for agents and search: the product, the maintaining organization, and
// the site itself. Kept in the root layout so every page carries it.
const structuredData = {
  "@context": "https://schema.org",
  "@graph": [
    {
      "@type": "SoftwareApplication",
      "@id": url("/#software"),
      name: site.name,
      alternateName: site.domain,
      url: url("/"),
      description: site.description,
      applicationCategory: "DeveloperApplication",
      applicationSubCategory: "Command Line Tool",
      operatingSystem: "macOS, Linux",
      softwareVersion: process.env.NEXT_PUBLIC_HORDE_VERSION ?? "0.3.1",
      downloadUrl: "https://horde.sh/install",
      installUrl: url("/docs"),
      softwareHelp: { "@type": "CreativeWork", url: url("/docs") },
      programmingLanguage: "Rust",
      license: site.licenseUrl,
      isAccessibleForFree: true,
      offers: { "@type": "Offer", price: "0", priceCurrency: "USD", availability: "https://schema.org/InStock" },
      author: { "@id": url("/#person") },
      publisher: { "@id": url("/#organization") },
    },
    {
      "@type": "Organization",
      "@id": url("/#organization"),
      name: site.name,
      alternateName: site.domain,
      url: url("/"),
      description: site.description,
      email: site.email,
      logo: { "@type": "ImageObject", url: url("/icon.svg") },
      founder: { "@id": url("/#person") },
      address: site.address,
      sameAs: [site.repository],
      contactPoint: [
        {
          "@type": "ContactPoint",
          contactType: "technical support",
          email: site.email,
          url: url("/contact"),
          availableLanguage: ["English"],
        },
        {
          "@type": "ContactPoint",
          contactType: "customer support",
          email: site.email,
          url: site.issues,
          availableLanguage: ["English"],
        },
      ],
    },
    {
      "@type": "Person",
      "@id": url("/#person"),
      name: site.author,
      email: site.email,
      url: url("/about"),
    },
    {
      "@type": "WebSite",
      "@id": url("/#website"),
      name: site.name,
      url: url("/"),
      description: site.description,
      inLanguage: "en",
      publisher: { "@id": url("/#organization") },
    },
  ],
};

export default function Layout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <head>
        <link rel="alternate" type="text/markdown" href={url("/index.md")} />
        <link rel="alternate" type="application/xml" href={url("/sitemap.xml")} title="Sitemap" />
        <link rel="help" href={url("/docs/agents")} title="Horde MCP and CLI reference" />
        <link rel="alternate" type="text/plain" href={url("/llms.txt")} title="Horde agent index" />
        <script
          type="application/ld+json"
          dangerouslySetInnerHTML={{ __html: JSON.stringify(structuredData) }}
        />
      </head>
      <body>
        {children}
        <footer className="site-footer">
          <span>© Wheel Go Fast, Inc.</span>
          <a href="https://gethamster.com">Get Hamster</a>
          <a href="https://x.com/HamsterResearch">Hamster Labs on X</a>
        </footer>
      </body>
    </html>
  );
}
