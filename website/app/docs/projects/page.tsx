import Link from "next/link";
import type { Metadata } from "next";
import { url } from "../../site";

const guides = "https://github.com/gethamster/horde/blob/main/docs";

export const metadata: Metadata = {
  title: "Work in a project",
  description: "Create a Horde project, register its repository, configure provider accounts and runtime access, and bind your agent to that project over MCP.",
  alternates: { canonical: url("/docs/projects") },
};

export default function Projects() {
  return <>
    <p><Link href="/docs">Set up Horde</Link> / Projects</p>
    <h1>Work in a project</h1>
    <p>Use a named project when you want separate tasks, configuration, and access to provider accounts and workers for each body of work. Several projects can share one Horde daemon.</p>
    <p>The <Link href="/docs">quick start</Link> uses the default project for an unregistered repository. Start here with Horde installed and a Git checkout that has an initial commit and a configured Git author.</p>

    <h2>1. Create a project and register its repository</h2>
    <pre><code>{`horde start
horde project create my-app --concurrency 2
horde project repo-add my-app /absolute/path/to/my-app
horde project runtime-grant my-app local
horde project inspect my-app`}</code></pre>
    <p>Replace <code>my-app</code> with your project slug and use your checkout’s absolute path. The <code>local</code> grant allows this daemon’s runtime to execute the project’s tasks. New projects begin with no runtime or provider account grants.</p>
    <p>A checkout and all its Git worktrees belong to one project. If you already used that checkout in the default project, use a separate clone for the new project. Repository ownership cannot be reassigned.</p>
    <p>To check scheduling before configuring a model provider, run a simulated task:</p>
    <pre><code>{`horde --project my-app submit "Check project scheduling" --repo /absolute/path/to/my-app --template simulated
horde --project my-app inspect TASK_ID`}</code></pre>
    <p>Replace <code>TASK_ID</code> with the returned task ID. The simulated template makes no model calls.</p>

    <h2 id="configuration">2. Configure the project’s agents</h2>
    <p>Named projects start with built-in settings and do not inherit your default project’s user configuration. This example uses the installed Claude Code CLI for planning, implementation, and review. Save this TOML in a private file outside the repository, such as <code>/private/path/my-app.toml</code>:</p>
    <pre><code>{`concurrency = 2

[executors.planner]
provider = "claude"

[executors.worker]
provider = "claude"

[executors.reviewer]
provider = "claude"`}</code></pre>
    <pre><code>{`horde project configure my-app --file /private/path/my-app.toml`}</code></pre>
    <p>Horde validates the file and stores it under <code>DATA_DIR/projects/PROJECT_ID/config.toml</code>. Continue using <code>horde config</code> for the default project. Repository workflow settings load afterward, but cannot define provider connections, raise concurrency, or expand account and runtime access.</p>

    <h2 id="accounts">3. Add a provider account</h2>
    <p>Named projects need managed accounts, even when a provider CLI is already signed in on the host. For the Claude configuration above, create an account and keep its returned ID:</p>
    <pre><code>{`horde --project my-app account create claude-main --provider claude --auth-mode login --base-url https://api.anthropic.com/v1 --concurrency 2
claude setup-token`}</code></pre>
    <p>Save the setup token in a private JSON file outside the repository, replacing the placeholder below. Use file permissions <code>0600</code> and include <code>expires_at</code> as Unix seconds when the expiration is known.</p>
    <pre><code>{`{
  "kind": "claude_setup_token",
  "secret": "REPLACE_WITH_SETUP_TOKEN"
}`}</code></pre>
    <pre><code>{`horde --project my-app account credential-set ACCOUNT_ID /private/path/claude-credential.json
horde --project my-app account inspect ACCOUNT_ID`}</code></pre>
    <p>Replace <code>ACCOUNT_ID</code> with the ID returned by account creation. Creating an account grants it to its owning project. Horde selects a granted account whose provider kind, authentication mode, and endpoint match the executor’s provider configuration.</p>
    <p>For Codex, API keys, and sharing an existing managed account, see <a href={`${guides}/configuration.md#separate-projects-and-provider-accounts`}>provider account configuration</a>. An administrator can grant an existing account with <code>horde account grant ACCOUNT_ID my-app</code>. Shared accounts keep one quota and concurrency total across projects.</p>

    <h2>4. Submit and follow project work</h2>
    <pre><code>{`horde --project my-app submit "Add CSV export with tests" --repo /absolute/path/to/my-app
horde --project my-app list
horde --project my-app inspect TASK_ID
horde --project my-app watch TASK_ID
horde --project my-app summary TASK_ID`}</code></pre>
    <p>Use the new task ID. The summary reports the task status and integrated commit. Horde integrates local work in a separate worktree and leaves your original checkout on its branch.</p>
    <p>Registered repositories supply the project when selection can be inferred. Passing <code>--project</code> explicitly keeps scripts and task commands scoped. To inspect all projects as an administrator, use <code>horde project list</code> and <code>horde list --all-projects</code> without <code>--project</code>.</p>
    <p>If work waits, inspect the task’s queue reason, then check <code>horde project inspect my-app</code> and <code>horde --project my-app account list</code>. A missing runtime grant, expired credential, or exhausted account capacity can prevent execution. See <a href={`${guides}/runtime-management.md#project-capacity-and-credential-lifetimes`}>capacity and credential troubleshooting</a>.</p>

    <h2 id="mcp">5. Connect an agent to this project</h2>
    <p>Give the agent a project-bound MCP connection:</p>
    <pre><code>{`{
  "mcpServers": {
    "horde-my-app": {
      "command": "horde",
      "args": ["--project", "my-app", "mcp"]
    }
  }
}`}</code></pre>
    <p>Use the absolute path to <code>~/.local/bin/horde</code> if your agent cannot find the command. For a custom data directory, include <code>"--data-dir", "/absolute/path"</code> before <code>"mcp"</code> and use that same directory in your CLI commands.</p>
    <p>The connection stays bound to this project. It cannot access another project’s tasks or artifacts, list all projects’ tasks, or change project, account-grant, and fleet administration. Run those administration commands from an unbound CLI or MCP connection. See the <Link href="/docs/agents">agent reference</Link> for operation schemas and worker credentials.</p>
    <p>Project boundaries protect Horde-managed requests and state. Programs running under the same OS account can still read each other’s files. Use a separate execution identity or VM isolation when you need a filesystem boundary.</p>

    <h2>When you need more</h2>
    <ul>
      <li><Link href="/docs/deployment/fleet#project-access">Grant access to remote workers</Link>: authorize the project on both hosts and keep its immutable project ID consistent.</li>
      <li><a href={`${guides}/runtime-management.md#optional-project-vms-with-lima`}>Use Lima VM isolation</a>: run project workers in separate guests.</li>
      <li><Link href="/docs/deployment/ax">Use an existing AX deployment</Link>: create project workers with optional Docker and Compose. Experimental.</li>
    </ul>
  </>;
}
