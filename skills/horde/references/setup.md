# Setup

## Requirements

macOS or Linux, Git, and a repository with at least one commit and a configured
Git author identity (`user.name` and `user.email`). The native Tuara executor
also uses `rg` (ripgrep) for its search tool. Docker is only needed for Compose
test environments and Docker-provisioned runtimes.

## Install

```sh
curl -fsSL https://horde.sh/install | bash -s -- --no-service
```

The installer downloads a signed release manifest, verifies its Ed25519 signature
and the artifact SHA-256, then installs. It links `horde` into `/usr/local/bin`
when that is writable, otherwise it adds `~/.local/bin` to your shell startup file
and tells you to open a new terminal. Versioned executables live under
`~/.local/share/horde-install`.

Pass `--service` or `--no-service` whenever an agent runs the installer. With
neither flag the boot-service choice defaults to `ask`, and the prompt is not
skipped just because stdin is a pipe: the script falls back to writing the
question to `/dev/tty` and reading the answer from it. Under `curl | bash` in an
agent tool call that blocks until a human types into the terminal, or until the
call times out. With an explicit flag the prompt is never reached.

Pinned to a version, or downloaded first so you can read it:

```sh
curl -fsSL https://horde.sh/install -o install.sh
sh install.sh --version 0.3.2 --no-service
```

From source (development, or an unpublished build):

```sh
cargo install --path . --locked
```

A source build has no embedded release verification key, so `horde update` will
report that it cannot verify updates. That is expected.

## Running the daemon

```sh
horde start     # detached; writes daemon.log in the data directory
horde daemon    # foreground
horde stop      # graceful; durable work is retained
```

`horde stop` kills each active process group. Durable state stays; resume with
`horde resume TASK_ID` where needed.

Start at machine boot:

```sh
horde service install     # systemd system service on Linux, LaunchDaemon on macOS
horde service status
horde service uninstall   # retains data and credentials
```

The service runs as the installing user with their HOME and a configured tool
PATH. Only install and uninstall use sudo. Provider credentials are not written
into world-readable service definitions; the service reads them from its daemon
environment or from a private `credentials.env` beside `config.toml` with mode
0600.

## Directories

| Purpose | Path |
| --- | --- |
| Data, worktrees, SQLite, `daemon.log` | `~/.local/share/horde` |
| User settings | `$XDG_CONFIG_HOME/horde/config.toml` or `~/.config/horde/config.toml` |
| App secret bundle map | `~/.config/horde/secrets.toml` |
| Runtime/fleet profiles | `~/.config/horde/runtimes.toml` |
| Network trust | `~/.config/horde/network.toml` |
| Project settings | `.horde.toml` in the submitted repository |
| Project templates | `.horde/templates/*.toml` |

Use `--data-dir /absolute/path` on **every** command to run a second instance.
The data directory holds a Unix socket, so keep the path short: roughly under 90
characters on macOS. The directory is mode 0700 and the socket 0600.

## Connect an agent over MCP

Canonical stdio MCP server definition:

```json
{
  "mcpServers": {
    "horde": { "command": "horde", "args": ["mcp"] }
  }
}
```

With a custom data directory the arguments are
`["--data-dir", "/absolute/path", "mcp"]`. If the agent cannot find the binary,
use the absolute path `~/.local/bin/horde` expanded.

Common wiring:

- Claude Code: `claude mcp add horde -- horde mcp`
- Cursor: the block above in `.cursor/mcp.json` or the global equivalent
- Codex CLI: `[mcp_servers.horde]` with `command = "horde"` and `args = ["mcp"]`
  in `~/.codex/config.toml`
- Anything else: any client that speaks newline-delimited stdio MCP

The bridge writes protocol messages only to stdout, supports initialization, tool
discovery, and tool calls, and exposes submit, inspect, events, questions, cancel,
resume, metrics, revisions, artifacts, knowledge, delegation, and coordination
tools. It has administrative authority over this Horde instance.

There is no separate Slack app, webhook, or chat store inside Horde. A bot or
personal agent stays the external caller through this same bridge, keeping its own
mapping from task ids to chat threads and using `events` plus `ack_events` with a
stable `consumer` string to resume cleanly after a reconnect.

## Verify the installation

```sh
horde doctor --repo /path/to/repo            # prints merged settings for that repo
horde doctor --repo . --probe-tuara          # checks the Tuara catalog and a live tool call
horde submit "Exercise the runtime" --repo . --template simulated
horde inspect TASK_ID
```

The `simulated` template exercises scheduling, worktrees, integration, and events
with no model calls and no cost. Run it before configuring a paid executor, and
again whenever you suspect the runtime rather than the model.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `unconfigured executor role X` at submit | The template names a role with no `[executors.X]` entry. Add it or change the template. |
| Submit fails on the repository | No initial commit, or no Git author identity. Commit once and set `user.name`/`user.email`. |
| Socket or path errors | Data directory path too long, or a stale daemon. Shorten `--data-dir`, or `horde stop` then `horde start`. |
| Provider auth failures in workers | The key was set in your shell, not the daemon's environment. Export it before `horde start`, or put it in `credentials.env` for a service install. |
| Native model rejected | Horde verifies the exact configured model identifier against `/models` and never substitutes an alias. Fix the identifier. |
| Task blocked after a restart | An uncertain attempt. Reconcile the worker, then resume. See `tasks.md`. |
| `horde update` says it cannot verify | Source build with no embedded release key, which is correct behavior. Use the official installer for managed updates. |
