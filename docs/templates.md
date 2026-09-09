# Authoring templates

Place versioned TOML in `.horde/templates/`. Inputs are strings substituted using `{{name}}`. Step outputs use `${step.field}` and must reference a direct dependency. Nested template invocations add namespaces; compiler-generated references and template output aliases are rewritten before validation. `output_types` can declare additional named JSON outputs and enforces their type at completion.

A parallel implementation:

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

Separate coding worktrees are allocated automatically. The two prefixes cannot conflict. Claims still matter for a shared file outside these scopes: a worker must acquire it before editing or ask its current owner to transfer it.

A bounded repair and verification loop:

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

If the first check succeeds, the repair branch is skipped. If it fails, the repair and subsequent verification must succeed. `attempts` repeats a step; a verification step needing an implementation repair should have an explicit failure branch. DAG cycles are rejected; bounded retries and explicit repair branches supply loops.

Explicit escalation in repository settings:

```toml
[fallbacks]
worker = "reviewer"
```

A step with `attempts = 2` tries its configured worker first and the configured reviewer after failure. No fallback happens without a mapping. Missing roles and fallback cycles are rejected. A role fallback is a configured change of executor, not a silent model alias.

The shipped Next.js template assumes npm and a committed `package-lock.json`: after local implementation/review it runs `npm ci`, the test script if present, and the production build in the integrated worktree. Override the template for pnpm, Yarn, a monorepo, or a custom validation sequence. Native/CLI coding workers install any dependencies they need in their own worktrees.

GitHub delivery is a separate opt-in step. For non-Next.js projects, include your local template and follow it with a `kind = "delivery"` step. Recovery for a transient external failure can retry the delivery step. A failed combined check that requires code changes should enter an explicit repair branch; the service does not invent an unbounded repair loop.


## Selecting runtime skills

Configure named directories under `[skills]` in Horde settings, then select names
with `skills = ["report"]` on an agent step. Each name must exist in the task's
pinned catalog. The runtime loads selected instructions into the prompt and makes
bundled resources available through `read_skill`. A nested template's `skills`
selection is propagated to its agent steps. See [runtime skills](runtime-skills.md)
for configuration, inheritance, and file limits.

## Proposing and appending steps through tools

`propose_steps` and `add_steps` publish a self-contained JSON schema derived from
Rust's `Step`, `Condition`, and `Environment` types. It is shared by MCP tool
listing, the native executor, and the generated website catalog. Unknown fields
are rejected. A step requires `id`; omitted `role`, `kind`, and `attempts` default
to `worker`, `agent`, and `1`. Other fields, including nested environment settings,
are described in the tool schema. Graph dependencies, configured roles, permissions,
and cross-field constraints are still checked by the runtime.

Call the tool named `propose_steps` with a `steps` array. Step IDs are data inside
that array, never tool names. For example, an active planner can send:

```json
{
  "steps": [
    {
      "id": "implement_json",
      "instructions": "Add JSON output and tests. Run checks and commit changes.",
      "scope": ["cli.py", "tests"],
      "tools": ["read_file", "search", "write_file", "apply_patch", "command"],
      "acceptance": ["Existing text output is unchanged", "JSON tests pass"]
    },
    {
      "id": "verify_json",
      "kind": "command",
      "needs": ["implement_json"],
      "command": ["python3", "-m", "unittest"],
      "when": {"step": "implement_json", "status": "succeeded"}
    }
  ]
}
```

Worker credentials supply task/worker identity. Operator calls must supply the
task and, for proposals, the active planner worker. Proposals accept 1–32 expanded
steps, automatically depend on the planner, and gate its pending successors.
They cannot include delivery steps or nested template invocations. `add_steps`
is the operator's append operation; it also takes Step objects and does not expand
nested templates. Compile nested templates before passing expanded steps.

Type errors identify the input path, for example `steps[0].needs: invalid type:
string ..., expected a sequence`. Unknown or missing fields retain Serde's field
names. Semantic errors identify the expanded workflow index and step ID, such as
`workflow.steps[4] (id="verify_json").needs: unknown dependency ...`. Inspect the
named step, correct the arguments, and call the same tool again. MCP returns
validation failures as `isError` tool results; the native executor sends a tool
error response and records the bounded error in events. Invalid proposals do not
create a revision or insert partial steps.

A step can override the [progress budget](configuration.md#step-progress-budgets):

```toml
[[steps]]
id = "implement"
role = "worker"
step_budget_seconds = 2400
instructions = "Implement and verify the change."
```

An override on a nested template inclusion supplies the default for its expanded
steps; a child's explicit value takes precedence. Each retry starts a fresh window.
