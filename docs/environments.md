# Application secrets and disposable environments

Configure the application once, then inherit its setup through the task tree.
Provider credentials stay in executor configuration and the credential broker;
app bundles are for the software being built and tested.

## Named private bundles

Create a private source file and map its name in user-owned
`~/.config/horde/secrets.toml` (or `$XDG_CONFIG_HOME/horde/secrets.toml`):

```toml
[bundles]
app = "/absolute/private/project.env"
```

The source file must have mode 0600. Its format is literal `KEY=VALUE`, optionally
prefixed with `export`; comments and whole single/double quotes are supported.
There is no shell interpolation, multiline syntax, or escape evaluation.
Duplicate names, NUL bytes, and reserved `HORDE_` or `TASK_` names are rejected. Keep
provider authentication keys out of app bundles.

Select it in user settings or the project's `.horde.toml`:

```toml
secret_bundles = ["app"]
```

A root binds bundle names and content versions. Children inherit the same whole
bundles by default; `delegate_task` can narrow them with `bundles: []` or a
subset. Children cannot expand access. SQLite stores names and hashes, not values.
Changed source files fail the pinned version check until the caller explicitly
uses `refresh_bundles` for the affected task. Existing descendants retain their
own pinned versions; refresh each intended descendant explicitly.

Selected values are injected into native commands, configured command steps,
combined validation, and managed app environments. Harness model processes do not
receive the bundle as command environment; their app testing uses managed steps.
A managed step also materializes a private `.env` in its worktree when absent,
adds it to the repository's local Git exclude file, and removes the owned file
on completion/recovery. Existing files are preserved. Set `env_file` to another
relative filename if the app expects one. Do not commit app credentials.

Remote transfers require both the caller's `share_bundles` and the executor's
`receive_bundles` grants, plus enrolled mTLS identities and execution permission.
Private temporary copies are removed after terminal cleanup and on daemon
restart. Active work fetches them again from the owning caller. Values do not go
into launch context, SQL records, or persisted command/app logs. Redaction covers
selected literal values, not arbitrary transformations produced by app code.

## Process app example

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

Adapt commands to the repository and ensure dependencies are installed in a
preceding step. `PORT` supplies an allocated ephemeral loopback port, and `${PORT}`
in start arguments is also expanded. The app must honor that port. Test commands
receive `HORDE_APP_URL`. Startup must return a successful HTTP status before
tests begin. The process runs in its own process group and is stopped on success,
failure, cancellation, or timeout.

Horde allocates an ephemeral loopback port and releases it immediately before
starting the app, so the port is briefly unowned. Concurrent environments cannot
collide there: a port stays reserved until its app has finished starting, and
allocation skips ports other starts are holding. A process outside Horde can
still take it, and when that happens the app exits before readiness and Horde
says the port was taken rather than reporting a plain startup failure, so it is
not mistaken for a defect in the app. Give the step `attempts = 2` to retry with
a fresh port. Process mode is not an OS sandbox and does not enforce the Compose
CPU/memory settings.

## Docker Compose app example

Use the project's existing `compose.yaml` and this environment block:

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
```

Horde runs the installed Docker CLI, renders a private resolved configuration,
assigns a unique project name and owned network/volume names, replaces published
ports with ephemeral loopback ports, and divides the configured resource budget
across services. Selected app values are supplied to services. It runs
`compose up --detach --wait`, discovers the published TCP port for `ready_service`
(or the first service with a TCP port), then checks `ready_url` at that port.

Stacks without published ports rely on Compose running/health status and receive
an empty `HORDE_APP_URL`. Their test command can execute inside the stack using
`HORDE_COMPOSE_PROJECT` and `HORDE_COMPOSE_FILE` with `docker compose exec`.
`services` optionally selects which services to start. An explicitly selected
readiness service should be among those services.

Fixed container names, host networking, and privileged containers are rejected.
Writable bind mounts, host devices, and external resources require explicit
`allow_external_resources = true`. These resources lie outside automatic
isolation; owned named volumes are preferable for disposable data. External
volumes are never deleted by Compose teardown.

The lifecycle records readiness/test evidence, captures redacted logs, and runs
`compose down --volumes --remove-orphans`. Log capture is bounded (process tail
1 MiB per stream; Compose tail 200 lines per container). Failed cleanup retains a
`cleanup_pending` record for retry. Docker must be available on the selected
context; Horde does not start or reconfigure Docker engines.

## Crash recovery

The daemon records app ownership and process identity before tests. After a hard
restart it stops matching app/test process groups, removes owned env files, and
tears down the owned Compose project. Identity mismatch is held for inspection,
so an unrelated process is not killed because its PID was reused. Uncertain step
effects still require normal worker reconciliation before retrying. A remote root
keeps environment reservations while cleanup cannot be confirmed.

The root allows two live environments by default. The default lifetime is thirty
minutes. These are small per-task testing environments, not an application
hosting platform or a replacement for production deployment.
