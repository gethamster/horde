# Template examples

All of these go in `.horde/templates/*.toml`. Validate each one with
`horde validate NAME --repo /path/to/repo` before submitting.

## The built-ins, and what they assume

**`local-implementation`** — the default. Three steps: `plan` (planner role,
read-only tools, may call `propose_steps`), `implement` (worker, scope `["."]`,
3 attempts), `review` (reviewer, scope `["."]`, 3 attempts, repairs concrete
defects and fails if acceptance is unmet). Its output alias is `review.result`.

**`nextjs`** — local implementation plus verification on the integrated worktree:
`npm ci`, the `test` script if present, then the production build. It assumes npm
and a committed `package-lock.json`. Override it for pnpm, Yarn, a monorepo, or a
different validation sequence.

**`github-actions`** — `nextjs` verification followed by a GitHub delivery step.
Delivery must also be enabled in settings; see the `horde` skill's delivery
reference.

**`simulated`** — exercises scheduling, worktrees, and integration with no model
calls. Use it to prove the runtime works before spending anything.

## Nested templates

A step with a `template` field expands that template inline. Its steps are
namespaced with the including step's id, and a `needs` on that step id resolves to
the nested template's terminal steps.

```toml
name = "feature-with-checks"
version = "1.0.0"
inputs = ["task"]

[outputs]
result = "build.result"

[[steps]]
id = "build"
template = "local-implementation"
inputs = { task = "{{task}}" }

[[steps]]
id = "lint"
kind = "command"
needs = ["build"]
command = ["npm", "run", "lint"]
```

The nested steps become `build.plan`, `build.implement`, `build.review`. `lint`
depends on whatever the nested template ends with. A nested template's own
`[outputs]` aliases are rewritten to the real task and field before validation, so
`${build.result}` works from the parent.

Two rules: a template cannot include itself at any depth, and `when` cannot be put
on a template-inclusion step. Put conditional execution on the child steps.

## Parallel workers with separate scopes

```toml
name = "parallel-feature"
version = "1.0.0"
inputs = ["task"]

[outputs]
result = "verify.result"

[[steps]]
id = "api"
role = "worker"
scope = ["src/api"]
tools = ["read_file", "search", "write_file", "apply_patch", "command"]
instructions = "Implement the API for {{task}}. Coordinate interfaces with the UI worker through messages, test and commit."
acceptance = ["The endpoint is implemented", "API tests pass"]

[[steps]]
id = "ui"
role = "worker"
scope = ["src/ui"]
tools = ["read_file", "search", "write_file", "apply_patch", "command"]
instructions = "Implement the UI for {{task}}. Coordinate interfaces with the API worker through messages, test and commit."
acceptance = ["The UI renders the new data", "UI tests pass"]

[[steps]]
id = "verify"
kind = "command"
needs = ["api", "ui"]
command = ["npm", "test", "--", "--run"]
```

`api` and `ui` get separate worktrees and run concurrently because their scope
prefixes cannot conflict. A shared file outside both prefixes still needs a claim,
acquired by one worker and transferred if the other needs it.

## Repair and re-verify

```toml
name = "repair-example"
version = "1.0.0"

[[steps]]
id = "check"
kind = "command"
command = ["cargo", "test"]
attempts = 2

[[steps]]
id = "repair"
role = "worker"
needs = ["check"]
scope = ["."]
tools = ["read_file", "search", "write_file", "command"]
instructions = "Read the failed check evidence in context, fix the cause, test, and commit."
[steps.when]
step = "check"
status = "failed"

[[steps]]
id = "verify"
kind = "command"
needs = ["repair"]
command = ["cargo", "test"]
```

## Chaining a plan into an implementation

`${step.field}` reads a named output from a direct dependency. Referencing a step
you do not `need` is rejected.

```toml
[[steps]]
id = "plan"
role = "planner"
tools = ["read_file", "search", "command"]
instructions = "Inspect the repository and plan this task: {{task}}. Do not edit files. Return a concrete implementation plan."
acceptance = ["A concrete implementation plan was produced"]

[[steps]]
id = "implement"
needs = ["plan"]
scope = ["."]
tools = ["read_file", "search", "write_file", "apply_patch", "command"]
instructions = "Implement {{task}}. Follow the dependency plan: ${plan.result}. Check messages before editing. Run relevant tests and commit."
attempts = 3
```

Declare extra named outputs and have their types enforced at completion:

```toml
[[steps]]
id = "plan"
role = "planner"
[steps.output_types]
result = "string"
risk = "string"
```

## App test environment

```toml
name = "app-test"
version = "1.0.0"
inputs = ["task"]

[[steps]]
id = "install"
kind = "command"
command = ["npm", "ci"]

[[steps]]
id = "app"
kind = "environment"
needs = ["install"]
acceptance = ["The running app passes its integration tests"]

[steps.environment]
runner = "process"
start = ["npm", "run", "dev", "--", "--hostname", "127.0.0.1"]
test = ["npm", "run", "test:integration"]
ready_url = "http://127.0.0.1:${PORT}/"
timeout_seconds = 1800
readiness_seconds = 60
```

Compose variant:

```toml
[steps.environment]
runner = "compose"
compose_file = "compose.yaml"
ready_service = "web"
services = ["web", "db"]
test = ["npm", "run", "test:integration"]
timeout_seconds = 1800
readiness_seconds = 60
memory_mb = 2048
cpus = 2.0
```

`PORT` is an allocated ephemeral loopback port; the app must honor it. Test
commands get `HORDE_APP_URL`, and Compose tests also get `HORDE_COMPOSE_PROJECT`
and `HORDE_COMPOSE_FILE`. Full behavior, isolation rules, and cleanup semantics are
in the `horde` skill's environments reference.

## Delivery

```toml
[[steps]]
id = "deliver"
kind = "delivery"
needs = ["verify"]
```

A `delivery` step does nothing unless `[delivery] enabled = true` is set in
configuration, and it will not merge unless `merge = true`. A failed delivery that
needs code changes belongs in an explicit repair branch.

## Errors you will hit

| Message | Cause |
| --- | --- |
| `unknown template X` | Not in `.horde/templates/` and not a built-in |
| `missing input X for Y` | The template declares `inputs` that the caller did not supply |
| `recursive template inclusion` | A template includes itself, directly or transitively |
| `unknown output step X` | `[outputs]` references a step id that does not exist at that level |
| `output must reference step.field` | An `[outputs]` value with no dot |
| `put conditional execution on child steps` | `when` on a template-inclusion step |
| `unconfigured executor role X` (at submit) | A step's `role` has no `[executors.X]` entry in the merged settings |
| unknown field errors | A misspelled key. Templates reject unknown fields |
