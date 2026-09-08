import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../../site";
export const metadata: Metadata = {
  title: "Configure",
  description: "Configure Horde settings files, executor roles, concurrency limits, account usage policy, and credentials.",
  alternates: { canonical: url("/docs/configuration") },
};

export default function Configuration() {
  return <>
    <h1>Configure</h1>
    <h2>Settings files</h2>
    <p>Run <code>horde config</code> to print starter TOML. User settings live in <code>~/.config/horde/config.toml</code>, or <code>$XDG_CONFIG_HOME/horde/config.toml</code>. A repository can override workflow settings in <code>.horde.toml</code>.</p>
    <p>Horde reads these settings when you submit a task. Later edits apply to new tasks.</p>
    <pre><code>{`concurrency = 4
autonomy = true
timeout_seconds = 1800

[executors.worker]
kind = "codex"

[executors.reviewer]
kind = "claude"`}</code></pre>
    <p>Codex and Claude roles use the installed CLI and its existing login by default. Configure and authenticate each execution host separately. Set <code>autonomy = false</code> to require an initial confirmation before work starts.</p>
    <h2>Concurrency</h2>
    <pre><code>{`horde config get concurrency
horde config set concurrency 8
horde runtime drain
horde runtime resume`}</code></pre>
    <p>Each runtime has its own persistent ceiling, from 1 to 64 workers. The default is four. Lowering the ceiling lets active work finish. Draining stops new invocations until you resume.</p>
    <h2>Account usage</h2>
    <pre><code>{`horde usage`}</code></pre>
    <p>Usage reports show available capacity observations, their source, and freshness. Missing quota data stays unknown. Subscription percentages are not inferred from token counts.</p>
    <p>Capacity policy lives in <code>~/.config/horde/runtimes.toml</code>:</p>
    <pre><code>{`[capacity_policy]
warn_percent = 80
switch_percent = 90
stale_seconds = 300`}</code></pre>
    <p>At the switch threshold, Horde follows your configured fallback chain. If every configured option is unavailable, work waits. For example, add this to <code>config.toml</code> to fall back from the worker role to the default Claude role:</p>
    <pre><code>{`[fallbacks]
worker = "claude"`}</code></pre>
    <h2>Credentials</h2>
    <p>For API-backed executors, configure <code>auth_mode = "api"</code>, <code>base_url</code>, and <code>api_key_env</code> on the role. Set the named key in the daemon environment before starting it. Boot services also read a private <code>credentials.env</code> beside <code>config.toml</code>; set that file’s permissions to <code>0600</code>.</p>
    <p>Keep credentials out of repository settings. Pairing a remote does not copy your subscription login or provider keys.</p>
    <h2>Run at startup</h2>
    <pre><code>{`horde service install
horde service status
horde service uninstall`}</code></pre>
    <p>Linux uses a systemd service; macOS uses a LaunchDaemon. Installation requests sudo and runs Horde as your user, preserving your selected configuration directory. Uninstalling the service retains data and credentials.</p>
    <Link className="next" href="/docs/deployment">Deploy remote workers →</Link>
  </>;
}
