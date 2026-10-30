# Service lifecycle and state

Use the owning repository’s runbook and supported lifecycle commands. Verify
services in the configured profile rather than assuming a packaging technology,
process manager, port scheme, or database layout.

## Establish ownership and identity

Identify the environment owner and which services the requested change affects.
For a shared development or test environment, coordinate changes before starting,
stopping, restarting, or reconfiguring services. Keep unrelated consumers running.
Do not launch a second instance merely because an existing one is hard to inspect.

Record only the service facts relevant to the check:

- The installed profile and supported lifecycle mechanism.
- The source revision, binary or artifact identity, and loaded configuration.
- The listener or endpoint actually serving the client.
- The service’s own state and the dependencies used by the affected operation.
- The authentication identity and audience, without recording credential values.

A file on disk is not proof that a process loaded it. Check the supported runtime
identity or diagnostic evidence after an authorized restart or reload. Where the
service cannot expose its revision, state the resulting verification limit.

## Use the actual configuration

Start from the installation’s declared configuration and environment loading.
Avoid ad hoc shell overrides that hide a broken fresh-start configuration. Check
required files and settings without printing their secret values. Keep runtime
configuration and data outside source-control and build inputs as required by
the owning repository.

Use addresses appropriate to the caller: a browser, a local process, a sandbox,
and a remote dependency may reach different endpoints. Preserve the configured
public origin, callback address, credential audience, and transport restrictions.
Changing a hostname or port may alter cookies or authentication behavior even
when it reaches the same machine.

Use the credential owner’s existing enrollment, refresh, or renewal mechanism.
Credentials must match the current environment and its identity records; an old
credential file does not establish a valid current account. Do not create an
independent refresh process, silently change identities, or bypass authentication
to obtain a passing check.

## Distinguish readiness from behavior

A process check establishes that the process exists. A listener check establishes
reachability. A dependency check establishes only the dependency behavior it
actually probes. None by itself establishes a successful authenticated workflow.
Use the documented meaning of each health response rather than assuming it covers
a database, model provider, queue, or downstream API.

If a dependency is unavailable, identify the affected operation and preserve
independent work. Repair or provision a remote dependency only within the user’s
authorization; a local task does not imply permission to change a shared remote
service or expose a private endpoint.

## Isolate changes and preserve state

Use distinct state locations, namespaces, listeners, and test identities for an
isolated installation. Inspect existing ownership before claiming any of them.
For stateful changes, keep service data, schema versions, and credentials aligned;
follow the owning repository’s migration and rollback procedure.

Keep compilation outputs separate from a running development service when a
build can overwrite assets or binaries that the service is using. Prefer the
supported build/install/restart sequence to replacing live files in place.

Stop task-owned services normally so pending cleanup can complete. Remove only
disposable state created for the check. A restart must not become a data reset,
and restoring an old executable alone may not roll back a schema change.
