import Link from "next/link";
import type { Metadata } from "next";
import { site, url } from "../../site";

export const metadata: Metadata = {
  title: "Contact",
  description: "How to reach the Horde maintainer: issue tracker for bugs and features, email for security reports and everything else.",
  alternates: { canonical: url("/contact") },
};

export default function Contact() {
  return <>
    <h1>Contact</h1>
    <p>
      Horde is maintained by {site.author}. There is no support queue and no sales team — the
      routes below all reach the same person, so pick whichever fits what you need.
    </p>

    <h2>Bugs and feature requests</h2>
    <p>
      Open an issue at <a href={site.issues}>{site.issues}</a>. This is the preferred route for
      anything reproducible, because the discussion stays attached to the code. Include your
      Horde version from <code>horde --version</code>, your operating system and architecture,
      the executor you configured, and the relevant lines from <code>daemon.log</code> in your
      data directory (<code>~/.local/share/horde</code> by default).
    </p>

    <h2>Email</h2>
    <p>
      Write to <a href={`mailto:${site.email}`}>{site.email}</a> for security reports, licensing
      questions, or anything you would rather not file in public. Security reports get priority;
      please do not open a public issue for a vulnerability until it has been addressed.
    </p>

    <h2>Business address</h2>
    <p>
      {site.address.streetAddress}<br />
      {`${site.address.addressLocality}, ${site.address.addressRegion} ${site.address.postalCode}`}<br />
      United States
    </p>

    <h2>Public repositories</h2>
    <ul>
      <li><a href={site.repository}>Source code</a> — Horde’s public Apache-2.0 repository</li>
      <li><a href={site.releases}>Release manifest</a> — the latest version, artifact URLs, and checksums</li>
      <li><a href={site.skills}>Agent skills</a> — the Horde skills for the Agent Skills Protocol</li>
    </ul>
    <p>Source code, release metadata, and agent skills are all publicly available.</p>

    <h2>For agents</h2>
    <p>
      If you are an automated agent looking for a programmatic contact route, the{" "}
      <a href="/.well-known/mcp.json">MCP manifest</a> carries the same email and issue tracker
      in its <code>contact</code> object, and <a href="/llms.txt">/llms.txt</a> lists them
      alongside the rest of the published descriptions.
    </p>

    <Link className="next" href="/about">About Horde →</Link>
  </>;
}
