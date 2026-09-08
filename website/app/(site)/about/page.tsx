import Link from "next/link";
import type { Metadata } from "next";
import { site, url } from "../../site";

export const metadata: Metadata = {
  title: "About",
  description: "Horde is an open-source local daemon that turns a task into a durable workflow and coordinates coding agents. Built and maintained by Andrew Somervell.",
  alternates: { canonical: url("/about") },
};

export default function About() {
  return <>
    <h1>About Horde</h1>
    <p>Source code: <a href={site.repository}>gethamster/horde</a>, licensed under {site.license}.</p>
    <p>
      Horde is a developer tool: a local Rust daemon that turns one objective into a durable
      workflow and coordinates coding agents against a Git repository. It is licensed under
      {" "}{site.license}, free to use, and distributed as signed releases you install with a
      single command from <a href={site.releases}>{site.releases}</a>.
    </p>

    <h2>What it is for</h2>
    <p>
      Coding agents are good at bounded edits and bad at surviving. A session ends, a laptop
      sleeps, a process is interrupted, and the work is gone. Horde puts durable state underneath
      that work. Every task, step, attempt, worker, message, artifact, and question is recorded
      in SQLite, so an interrupted run resumes from where it stopped rather than starting again.
      Workers run in isolated Git worktrees and claim exclusive file paths, which is what makes
      running several of them against one repository safe.
    </p>
    <p>
      It is deliberately local-first. The daemon runs on your own machine and drives the coding
      agent CLIs you already have installed and authenticated. There is no hosted service, no
      account, and no upload of your source. Remote execution is opt-in: you pair additional hosts
      over your own Tailscale network, each with its own data directory, concurrency ceiling, and
      executor credentials.
    </p>

    <h2>How it is built</h2>
    <p>
      Horde is written in Rust and ships as a single binary for macOS and Linux on Apple Silicon /
      ARM64 and x86-64. Coordination state is SQLite. Host-to-host communication uses mutually
      authenticated gRPC over TLS with per-host certificates. Agents integrate through a stdio
      bridge that speaks the Model Context Protocol, exposing the same operations the CLI uses.
    </p>
    <p>
      Releases are signed with an Ed25519 key whose public half is published at{" "}
      <a href="/release-key.pem">/release-key.pem</a> and pinned by fingerprint at{" "}
      <a href="/release-key.hex">/release-key.hex</a>. The installer verifies the signature and
      checksum before it installs anything, without requiring you to install OpenSSL first.
    </p>

    <h2>Agent skills</h2>
    <p>
      Horde ships three skills for the Agent Skills Protocol at{" "}
      <a href={site.skills}>{site.skills}</a>: <code>horde</code> for an agent driving Horde,{" "}
      <code>horde-templates</code> for authoring workflow templates, and <code>horde-worker</code>{" "}
      for an agent running inside a Horde task. Install them with <code>{site.skillsInstall}</code>.
    </p>

    <h2>Maintainer</h2>
    <p>
      Horde is built and maintained by {site.author}. Bug reports and feature requests belong on{" "}
      <a href={site.issues}>the issue tracker</a>; see <Link href="/contact">contact</Link> for
      other ways to get in touch.
    </p>
    <p>
      Horde is an early release: parts of it have been exercised against real services and parts
      only against fixtures. Treat it accordingly, and report anything that surprises you.
    </p>

    <Link className="next" href="/docs">Set up Horde →</Link>
  </>;
}
