import Link from "next/link";
import type { Metadata } from "next";
import { site, url } from "../../site";

export const metadata: Metadata = {
  title: "Privacy",
  description: "What horde.sh and the Horde daemon collect: no analytics on the website, no telemetry in the daemon, and no source code leaves your machine.",
  alternates: { canonical: url("/privacy") },
};

export default function Privacy() {
  return <>
    <h1>Privacy</h1>
    <p>
      Short version: this website runs no analytics and sets no cookies, and the Horde daemon
      sends no telemetry. Your source code never leaves the machines you run it on.
    </p>

    <h2>This website</h2>
    <p>
      {site.domain} is a static site. It contains no analytics script, no advertising, no
      third-party embeds, and no tracking pixels, and it sets no cookies. Nothing on the site asks
      you to create an account or submit personal information. The install command copy button
      runs entirely in your browser using the Clipboard API; nothing is transmitted.
    </p>
    <p>
      The site is hosted on Vercel, which records standard server request logs — IP address, time,
      requested path, and user agent — for delivery and abuse prevention. The installer and
      release downloads redirect to GitHub Releases, so GitHub also sees those requests under its
      own privacy policy. Neither log is used by this project for analytics or profiling.
    </p>

    <h2>The Horde daemon</h2>
    <p>
      Horde runs locally. It reports no usage statistics, sends no crash reports, and makes no
      network calls except the ones you configure: fetching a signed update when you run{" "}
      <code>horde update</code>, reaching the coding agent providers you configured, reaching
      hosts you paired yourself, and delivery to a Git remote if you explicitly enabled it.
    </p>
    <p>
      All coordination state — objectives, steps, events, messages, artifacts, and knowledge —
      stays in a SQLite database in your data directory, <code>~/.local/share/horde</code> by
      default. Nobody but you can read it. Provider credentials are read from your environment or
      from a private <code>credentials.env</code> beside your configuration; they are not copied
      into task state and are not transferred to hosts you pair.
    </p>
    <p>
      When you submit a task, the objective and the relevant repository contents are sent to
      whichever coding agent provider you configured, exactly as they would be if you invoked that
      agent CLI yourself. Those providers process that data under their own terms. Horde adds no
      intermediary and stores no copy off your machine.
    </p>

    <h2>Questions</h2>
    <p>
      Email <a href={`mailto:${site.email}`}>{site.email}</a> with any privacy question, or see{" "}
      <Link href="/contact">contact</Link> for other routes. If this policy changes, the change
      will appear in the repository history alongside the rest of the site.
    </p>

    <Link className="next" href="/about">About Horde →</Link>
  </>;
}
