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
