import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../../site";
export const metadata: Metadata = {
  title: "Configure",
  description: "Configure Horde providers, Responses and Chat Completions workers, Jev decisions, executor roles, and credentials.",
  alternates: { canonical: url("/docs/configuration") },
};

export default function Configuration() {
  return <>
    <h1>Configure</h1>
    <h2>Settings files</h2>
    <p>Run <code>horde config</code> to print starter TOML. User settings live in <code>~/.config/horde/config.toml</code>, or <code>$XDG_CONFIG_HOME/horde/config.toml</code>. A repository can override workflow settings in <code>.horde/horde.toml</code>. The legacy <code>.horde.toml</code> also loads; the nested file wins when both set the same value.</p>
    <p>Horde reads these settings when you submit a task. Later edits apply to new tasks.</p>
    <p>These user settings apply to the default project. For a named project, follow <Link href="/docs/projects#configuration">project configuration and provider accounts</Link>; each project needs its own configuration and access grants.</p>
    <pre><code>{`concurrency = 4
autonomy = true
timeout_seconds = 1800

[executors.worker]
provider = "codex"

[executors.reviewer]
provider = "claude"`}</code></pre>
    <p>Codex and Claude roles use the installed CLI and its existing login by default. Configure and authenticate each execution host separately. Set <code>autonomy = false</code> to require an initial confirmation before work starts.</p>
    <h2 id="model-protocols">Model protocols and when to use them</h2>
    <p>Horde uses generative models to carry out coding steps and decision models to answer bounded questions about the work. Choose a provider with the protocol your executor supports:</p>
    <div role="region" aria-label="Model protocol comparison" tabIndex={0} style={{ overflowX: "auto" }}>
    <table style={{ width: "100%", minWidth: 560, textAlign: "left", borderSpacing: "12px 16px" }}>
      <thead><tr><th scope="col">Protocol</th><th scope="col">Horde integration</th><th scope="col">Use it for</th></tr></thead>
      <tbody>
        <tr><td>Responses</td><td><code>kind = "codex"</code> with <code>auth_mode = "api"</code>, through the installed Codex CLI.</td><td>Coding steps that use the Codex harness and a Responses-compatible API.</td></tr>
        <tr><td>Chat Completions</td><td><code>kind = "tuara"</code>, through Horde’s native tool loop at <code>/chat/completions</code>.</td><td>Coding steps through Tuara or a compatible local or hosted model server.</td></tr>
        <tr><td>Messages</td><td><code>kind = "claude"</code> with <code>auth_mode = "api"</code>, through the installed Claude CLI.</td><td>Coding steps that use the Claude harness and an Anthropic-compatible API.</td></tr>
        <tr><td>Decisions (SystemOne v1)</td><td>A separate <code>[decision]</code> configuration; Tuara uses <code>/router/v1/systemone</code>.</td><td>Typed choices, scores, and assessments from 0 to 1 for routing and review advice, with optional native context and browser features.</td></tr>
      </tbody>
    </table>
    </div>
    <p>Here, “completions” means Chat Completions. Horde has no adapter for the legacy text <code>/completions</code> endpoint. The native executor uses Chat Completions; Responses and Messages run through their respective CLI harnesses. A decision request has no coding tools and does not replace a worker.</p>
    <h3>Native coding through Tuara</h3>
    <p>Define the endpoint and credential source on a provider, then assign that provider to a role. Select a generative model from the provider’s catalog:</p>
    <pre><code>{`[providers.tuara]
kind = "tuara"
auth_mode = "api"
base_url = "https://tuara.com/router/v1"
api_key_env = "TUARA_API_KEY"
model = "your-generative-model-id"
stream = true

[executors.worker]
provider = "tuara"`}</code></pre>
    <p>Horde appends <code>/chat/completions</code> to this base URL. A local compatible server can use <code>http://127.0.0.1:8122/v1</code> instead. With <code>model = "auto"</code>, Horde selects the sole model in <code>/models</code>; a catalog with multiple models requires an explicit ID.</p>
    <pre><code>{`horde config models tuara
horde doctor --provider tuara
horde doctor --probe --provider tuara`}</code></pre>
    <p>The probe makes a model request to check streamed tool calls and consumes provider capacity.</p>
    <h3>Responses through Codex</h3>
    <pre><code>{`[providers.openai]
kind = "codex"
auth_mode = "api"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[executors.worker]
provider = "openai"`}</code></pre>
    <p>Horde configures the Codex harness to use Responses. For Messages through Claude, use a provider with <code>kind = "claude"</code>, <code>auth_mode = "api"</code>, the Anthropic-compatible base URL, and its key variable. The built-in <code>codex</code> and <code>claude</code> providers continue to use subscription login.</p>
    <h3 id="decisions">Jev decisions through Tuara</h3>
    <p>Decision requests are disabled by default. Add this separately to your user configuration to record routing advice and work-product reviews while your coding workers continue to execute steps:</p>
    <pre><code>{`[decision]
mode = "shadow"
backend = "tuara"
base_url = "https://tuara.com/router"
api_key_env = "TUARA_API_KEY"
model = "XXXXTSJV130XXX"
protocol = "systemone-v1"
review_enabled = true
deadline_ms = 30000

[[decision.capability_guidance]]
runtime = "local"
capability = "worker"
description = "Use for repository changes assigned to the worker role."`}</code></pre>
    <p>Horde appends <code>/v1/systemone</code> to the decision base URL, which ends at <code>/router</code>. The example uses Jev’s Tuara catalog ID. Both the coding provider and decision provider can reference <code>TUARA_API_KEY</code>, but their endpoints and models are configured separately.</p>
    <p>Give each eligible runtime and role its own <code>capability_guidance</code> entry. Horde considers only available candidates already allowed by the task’s execution policy; missing guidance produces an abstention. Repository settings cannot enable or change decisions.</p>
    <p>In shadow mode, routing and review results are advisory. They do not select a different executor or authorize a merge. Inspect requests, abstentions, and outcomes with <code>horde decisions TASK_ID</code>, and work-product reviews with <code>horde reviews TASK_ID</code>. The <code>horde metrics TASK_ID</code> command includes decision usage and latency.</p>
    <p>Native context pruning and browser testing are separate opt-ins through <code>native_context_mode</code> and <code>browser_test_mode</code>. Each defaults to <code>"disabled"</code>; <code>"shadow"</code> records proposals, while <code>"active"</code> applies bounded actions within Horde’s checks. These features require <code>decision.mode = "shadow"</code> and do not rewrite external Codex or Claude sessions. Automatic delivery has a separate policy and requires an enrolled qualification. Qualification enrollment is not yet supported, so the live Jev smoke test does not enable autonomous merges.</p>
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
    <p>For API-backed executors, configure <code>kind</code>, <code>auth_mode = "api"</code>, <code>base_url</code>, and <code>api_key_env</code> on the provider. Each role selects it with <code>provider = "name"</code>. Set the named key in the daemon environment before starting it. Boot services also read a private <code>credentials.env</code> beside <code>config.toml</code>; set that file’s permissions to <code>0600</code>.</p>
    <p>Keep credentials out of repository settings. Pairing a remote does not copy your subscription login or provider keys.</p>
    <p>For an existing Tuara account, import its inference key with <code>horde config provider add tuara</code>, or ask your connected agent to use <code>provider_login</code>. That flow guides you to Tuara’s key page, verifies the key you supply, and saves it.</p>
    <h3 id="tuara-signup">Create a funded Tuara account</h3>
    <p>Horde can create a Tuara organization, fund it through Stripe’s Link wallet, and save its new inference key privately. Signup uses Tuara’s <a href="https://tuara.com/docs/agents/signup/index.md">Machine Payments Protocol endpoint</a>. You do not need to copy the new key or write operation JSON.</p>
    <p>Your connected agent first inspects the private Link wallet connection. If Link is missing, Horde uses the host’s Node.js and npm to install a pinned Link CLI under its configuration directory. If either prerequisite is missing, the agent receives a clear setup action. After installation succeeds, it starts device login and relays Link’s verification URL and phrase. The agent checks the session until it finishes and reads safe wallet readiness before starting signup.</p>
    <p>Link keeps payment methods in its <a href="https://app.link.com/wallet">hosted wallet</a>. When Horde reports a missing payment method or verification requirement, open that wallet and complete the action there. Link’s agent wallet currently supports US accounts. Horde’s MCP tools never receive card numbers or security codes. You can reuse a Link account connected to Grok Bot, but Horde does not register or configure Link MCP support in Grok Bot.</p>
    <p>Signup is available to an unbound default-project administrative connection; worker tokens and project-scoped connections cannot use it.</p>
    <p>Run the guided command to authorize the initial credit, maximum total charge including fees, and a specific <a href="https://tuara.com/terms/">Tuara terms version</a>:</p>
    <pre><code>{`horde config provider signup tuara`}</code></pre>
    <p>The walkthrough proposes $20 credit with a $20.48 total-charge ceiling. The minimum credit is $5, and Link limits the total charge to $500 including fees. An existing provider key requires <code>--replace-existing</code>. Horde preserves the provider’s model and role assignments.</p>
    <p>When wallet setup or approval needs more time, continue with <code>horde config provider signup tuara --request-id REFERENCE</code>. Link may require approval for the individual payment in its app. Only successful completion means the verified key is ready for the next invocation; model capacity remains unknown.</p>
    <p>Reuse the same request ID after a lost reply or daemon restart. Private receipts in <code>provider-signups/</code> beside your configuration preserve progress and the signup response. If payment’s outcome is <code>uncertain</code>, Horde puts later payment work on hold until you reconcile it with Tuara and your wallet; it will not charge again or create another account. Before paid submission, use <code>cancel</code> with the same request ID to stop signup. Cancellation cannot reverse a submitted payment.</p>
    <h3 id="tuara-topups">Automatic Tuara top-ups</h3>
    <p>Signup does not enable recurring charges. Configure a separate policy with an explicit threshold, credit amount, maximum total per charge including fees, UTC calendar-month limit including fees, terms version, and recurring authorization:</p>
    <pre><code>{`horde config provider topup tuara`}</code></pre>
    <p>Horde checks the balance every 60 seconds and waits five minutes after a successful charge. Link may require approval for each payment. Inspect, advance, or disable the policy with:</p>
    <pre><code>{`horde config provider topup tuara --status
horde config provider topup tuara --check
horde config provider topup tuara --disable`}</code></pre>
    <p>The monthly budget includes fees and is shared by provider aliases for the same Tuara origin and organization within one Horde configuration directory. It does not include spending on other machines or outside this policy. An uncertain payment holds future charges until you reconcile it with Tuara and Link. Disabling stops unpaid work but cannot reverse a submitted payment.</p>
    <p>Signup and top-ups have been tested with mock services and wallet processes. A paid live funding flow has not been validated.</p>
    <h2>Run at startup</h2>
    <pre><code>{`horde service install
horde service status
horde service uninstall`}</code></pre>
    <p>Linux uses a systemd service; macOS uses a LaunchDaemon. Installation requests sudo and runs Horde as your user, preserving your selected configuration directory. Uninstalling the service retains data and credentials.</p>
    <Link className="next" href="/docs/deployment">Deploy remote workers →</Link>
  </>;
}
