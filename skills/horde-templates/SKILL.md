---
name: horde-templates
description: Author, validate, and debug Horde workflow templates — the versioned TOML that turns an objective into a dependency graph of planning, coding, command, app-environment, and delivery steps. Use when writing or editing a file under .horde/templates/, when a Horde task needs parallel workers with separate write scopes, a bounded repair-and-verify loop, a nested sub-template, typed step outputs, a role fallback, or an app test environment, and when horde validate or horde submit rejects a template.
---

# Horde templates

A template is versioned TOML that compiles into a dependency graph. Horde pins the
template's content hash and the expanded plan to every task that uses it, so a
result can always be traced back to the exact workflow that produced it.

Project templates live in `.horde/templates/*.toml` and may add to or override the
built-ins: `local-implementation` (plan, implement, review), `nextjs`,
`github-actions`, and `simulated`.

Always validate before submitting. It costs nothing and catches every structural
error:

```sh
horde validate my-template --repo /path/to/repo
horde validate my-template --repo /path/to/repo --objective "a realistic objective"
```

## Shape

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
instructions = "Implement the API for {{task}}. Coordinate interfaces with the UI worker, test and commit."
acceptance = ["The API endpoint exists and its tests pass"]

[[steps]]
id = "ui"
role = "worker"
scope = ["src/ui"]
tools = ["read_file", "search", "write_file", "apply_patch", "command"]
instructions = "Implement the UI for {{task}}. Coordinate interfaces with the API worker, test and commit."

[[steps]]
id = "verify"
kind = "command"
needs = ["api", "ui"]
command = ["npm", "test", "--", "--run"]
```

Unknown keys are a hard error at load. So are missing inputs, duplicate step ids,
a `needs` entry that names no step, a dependency cycle, and a template that
includes itself.

## Step fields

| Field | Default | Meaning |
| --- | --- | --- |
| `id` | required | Unique within the template |
| `kind` | `agent` | `agent`, `command`, `delivery`, `environment`, `simulated` |
| `role` | `worker` | Executor role; must exist in settings as `[executors.<role>]` |
| `instructions` | `""` | Prompt for an agent step; `{{input}}` and `${step.output}` are substituted |
| `acceptance` | `[]` | Criteria the step is judged against. Write them so failure is detectable |
| `needs` | `[]` | Dependency step ids |
| `scope` | `[]` | Write scope; `["."]` is the whole repository |
| `tools` | `[]` | Native tool allowlist for an agent step |
| `artifacts` | `[]` | Expected artifact names |
| `output_types` | `{}` | Named JSON outputs and their types, enforced at completion |
| `command` | `[]` | argv for a `command` step |
| `environment` | none | Managed app environment block for an `environment` step |
| `template` | none | Nested template to invoke |
| `inputs` | `{}` | Inputs passed to a nested template |
| `when` | none | `{ step = "...", status = "..." }` condition on a dependency's terminal status |
| `attempts` | `1` | Bounded retries; each attempt keeps separate evidence |

Native tools available to an `agent` step on the Tuara executor: `read_file`,
`search`, `write_file`, `apply_patch`, `command`.

## Substitution

- `{{name}}` substitutes a template input. `{{task}}` is the submitted objective.
- `${step.field}` supplies a named output from a **direct** dependency. A reference
  to a step you do not depend on is rejected.
- Nested templates namespace their steps, and compiler-generated references and
  output aliases are rewritten before validation.

## Concurrency

Steps whose dependencies are satisfied run concurrently when their write scopes
permit it. Two workers with non-overlapping `scope` prefixes get separate worktrees
and run at the same time. Overlapping prefixes serialize.

Scope is not a substitute for claims. A worker that needs a shared file outside its
scope must acquire the claim first or ask the current owner to transfer it.

## Loops

DAG cycles are rejected. Iteration comes from two explicit mechanisms:

- `attempts = N` repeats a step up to N times.
- A `when` failure branch does a repair followed by another verification step.

```toml
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

If `check` succeeds, `repair` is skipped and its dependents are skipped with it. If
it fails, `repair` and `verify` must both succeed. A verification step that can be
fixed by an implementation change should always have an explicit repair branch;
Horde will not invent an unbounded repair loop for you.

## Role fallback

```toml
[fallbacks]
worker = "reviewer"
```

With `attempts = 2`, the first attempt uses `worker` and the second uses
`reviewer`. Fallbacks are opt-in, cycle-checked, and recorded as escalation
events. This is a configured change of executor, not a silent model alias.

## Evolving a workflow at runtime

- `add_steps` appends a validated revision from the caller without rewriting
  earlier attempts.
- `propose_steps` lets an active planner step propose parallel or dependent steps;
  the runtime validates the graph and inserts it before that planning step's
  pending successors.

Use these to add verification after inspecting evidence, rather than replaying
completed implementation or delivery work.

## More

`references/examples.md` has a full app-test template, a nested template, a
delivery template, and the built-in templates' assumptions.
