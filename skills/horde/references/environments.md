# App secrets and disposable environments

Configure the application once, then inherit that setup through the task tree.
These bundles are for the software being built and tested. Provider API keys stay
in executor configuration and the credential broker, and Horde rejects reserved
`HORDE_` names inside a bundle.

## Named private bundles

Create a private source file with mode 0600, then map its name in user-owned
`~/.config/horde/secrets.toml` (or the `$XDG_CONFIG_HOME` equivalent):

```toml
[bundles]
app = "/absolute/private/project.env"
```

The source format is literal `KEY=VALUE`, optionally prefixed with `export`, with
comments and whole single or double quotes supported. There is no shell
interpolation, no multiline syntax, and no escape evaluation. Duplicate names and
NUL bytes are rejected.

Select it in user settings or the project's `.horde/horde.toml`:

```toml
secret_bundles = ["app"]
```

A root binds bundle names and content versions. Children inherit whole bundles by
default; `delegate_task` can narrow with `"bundles": []` or a subset, never
widen. SQLite stores names and hashes, never values.

Changing the source file fails the pinned version check until the caller
explicitly refreshes:

```sh
horde call refresh_bundles '{"task":"TASK_ID"}'
```

Existing descendants keep their own pinned versions. Refresh each intended
descendant explicitly.

Values are injected into native commands, configured command steps, combined
validation, and managed app environments. Harness model processes do not receive
the bundle in their command environment; their app testing goes through managed
steps. A managed step also materializes a private `.env` in its worktree when one
is absent, adds it to the repository's local Git exclude file, and removes the
file it owns on completion or recovery. Existing files are preserved. Set
`env_file` to another relative filename if the app expects one.

Redaction covers the selected literal values in app logs and command evidence. It
cannot cover arbitrary transformations produced by application code.

Remote transfer requires the caller's `share_bundles` grant, the executor's
`receive_bundles` grant, enrolled mTLS identities, and execution permission.
Private temporary copies are removed after terminal cleanup and on daemon restart,
and refetched from the owning caller for still-active work.

## Process environment

Save as `.horde/templates/app-test.toml`:

```toml
name = "app-test"
version = "1.0.0"
inputs = ["task"]

[[steps]]
id = "app"
kind = "environment"
acceptance = ["The running app passes its integration tests"]

[steps.environment]
runner = "process"
start = ["npm", "run", "dev", "--", "--hostname", "127.0.0.1"]
test = ["npm", "run", "test:integration"]
ready_url = "http://127.0.0.1:${PORT}/"
timeout_seconds = 1800
readiness_seconds = 60
```

```sh
horde validate app-test --repo /path/to/repo
horde submit "Verify the running app" --repo /path/to/repo --template app-test
horde call environments '{"task":"TASK_ID"}'
```

`PORT` is an allocated ephemeral loopback port and `${PORT}` is expanded in start
arguments; the app must honor it. Test commands receive `HORDE_APP_URL`. Startup
must return a successful HTTP status before tests begin. The app runs in its own
process group and is stopped on success, failure, cancellation, or timeout.

Install dependencies in a preceding step. Process mode is not an OS sandbox and
does not enforce the Compose CPU and memory settings.

## Optional Jev-guided browser test

An environment can run a bounded browser test before its ordinary `test` command.
Add a checked-in browser spec and set `browser_test` in the environment step:

```toml
[steps.environment]
runner = "process"
start = ["npm", "run", "dev", "--", "--hostname", "127.0.0.1"]
browser_test = ".horde/browser-test.json"
test = ["npm", "run", "test:e2e"] # existing browser suite, used on fallback
ready_url = "http://127.0.0.1:${PORT}/"
```

For example, `.horde/browser-test.json` can contain:

```json
{
  "version": 1,
  "objective": "Save a note and confirm it appears",
  "assertions": [{ "kind": "text_visible", "text": "Saved" }],
  "values": { "Note": "test fixture text" },
  "max_steps": 12,
  "timeout_seconds": 120
}
```

Install an exact, project-pinned `playwright` package at version 1.48 or newer and its Chromium browser in
a preparatory step (`npx playwright install chromium`). The installed Horde binary
contains the driver; no Horde source checkout is needed in the app repository.
Set the operator's `decision.mode = "shadow"`, `decision.browser_test_mode = "active"`,
and TypeSafe credential as described in [configuration](configuration.md). The
key stays in the daemon; the driver protocol carries the app URL, declared
fixture values, and assertions. `browser_test_mode = "shadow"` records one
advisory choice and then runs the existing test command. The default is disabled,
which runs that command without parsing the browser spec or requiring a clean Git
worktree.

The driver observes visible, enabled buttons, links, inputs, and selects and
offers Jev one choice over compatible operation and control IDs. Fill and select
values must be declared in the spec under the control's accessible label. Horde
rejects stale IDs, cross-origin HTTP and WebSocket requests, and oversized action catalogs. Jev's
`DONE` choice only starts the independent URL-path or visible-text assertions.
A failed assertion fails the environment step. Unsupported controls, low-confidence
decisions, and repeated stalls run the configured `test` command. Keep that
fallback as a complete browser test, since this DOM runner does not invoke
application/WebMCP tools or interpret screenshots with a vision model.

Active browser tests require a clean committed worktree before and after
execution. The environment records that commit, a redacted decision trace, and a
screenshot in its private artifact store. If an application bundle contains
secrets, Horde omits screenshot capture and storage because pixels cannot be
safely redacted. Each Jev request counts against the operator's per-task decision
limit. The decision ledger records proposals and usage; a separate action receipt
marks a choice applied only after the driver acknowledges it and links to the
trace artifact. A lost acknowledgement remains pending for inspection. Screenshots
are evidence for a person or a separate vision-capable verifier; Jev receives
textual control metadata only.

## Docker Compose environment

```toml
[steps.environment]
runner = "compose"
compose_file = "compose.yaml"
ready_service = "web"
test = ["npm", "run", "test:integration"]
timeout_seconds = 1800
readiness_seconds = 60
memory_mb = 2048
cpus = 2.0
# docker_context = "colima"
# services = ["web", "db"]
```

Horde runs the installed Docker CLI, renders a private resolved configuration,
assigns a unique project name with owned network and volume names, replaces
published ports with ephemeral loopback ports, and divides the resource budget
across services. It runs `compose up --detach --wait`, discovers the published TCP
port for `ready_service` (or the first service with one), then checks `ready_url`
at that port.

Stacks with no published ports fall back to Compose running and health status and
get an empty `HORDE_APP_URL`. Their tests can run inside the stack using
`HORDE_COMPOSE_PROJECT` and `HORDE_COMPOSE_FILE` with `docker compose exec`.

Rejected outright: fixed container names, host networking, privileged containers.
Requiring explicit `allow_external_resources = true`: writable bind mounts, host
devices, external resources. Those lie outside automatic isolation, and external
volumes are never deleted by teardown. Prefer owned named volumes for disposable
data.

The lifecycle records readiness and test evidence, captures redacted logs (process
tail 1 MiB per stream, Compose tail 200 lines per container), and runs
`compose down --volumes --remove-orphans`. Failed cleanup leaves a
`cleanup_pending` record for retry. Docker must already be available on the
selected context; Horde does not start or reconfigure Docker engines.

## Crash recovery

Horde records app ownership and process identity before running tests. After a
hard restart it stops matching app and test process groups, removes owned env
files, and tears down the owned Compose project. An identity mismatch is held for
inspection so an unrelated process that reused a PID is not killed. Uncertain task
effects still need normal worker reconciliation before retrying, and a remote root
keeps environment reservations while cleanup cannot be confirmed.

The root allows two live environments by default with a thirty-minute lifetime.
These are small per-task test environments, not an application hosting platform
and not a substitute for a real deployment.
