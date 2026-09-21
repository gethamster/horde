---
name: horde-setup
description: Set up Horde providers, a controller, or fleet workers through agent_setup and existing platform access, then check the readiness needed for the requested work.
---

Use the caller's available tools to complete authorized setup. Keep ports, JSON,
and internal IDs inside tool calls; ask the user only for a missing preference,
credential access, or interactive login that the agent cannot supply. Existing
authorization remains sufficient for actions within that scope.

When the user asks to make Horde the default for repository work, use
`horde init --agent codex --delegate always` or
`horde init --agent claude --delegate always`, matching the caller. This installs
repository skills, instructions, and MCP configuration. It does not select worker
providers. Read the report, complete authorized `next_steps`, and reload the
caller's instructions and MCP configuration as needed. For other MCP clients,
configure their bridge and persistent instructions through their supported setup.
A worker already assigned a Horde step performs that assignment instead of
initializing a new caller or submitting another root task.

For provider, controller, or fleet setup, start with `agent_setup` action `inspect`. Follow relevant `next_actions`: invoke
entries with `kind:tool` through the named tool, and run `kind:exec` argv with the
calling agent's available execution tool on the indicated machine. Resolve a
reported blocker before retrying that action. Inspect again after a state change;
repeating an unchanged inspection does not resolve authentication or connectivity.

For providers, use action `configure_provider` with the requested provider and
executor `roles`. Preserve existing settings outside the requested change. For a
custom provider, discover its supported `kind`, authentication mode, `base_url`,
concrete `model`, and `api_key_env` from existing configuration or official provider
information. When the user gives the trusted agent an API key, pass it as
`credential`. Use `credential_env` or `credential_file` when a reference is already
available; these three sources are mutually exclusive. Do not echo the key in
responses. A preset does not prove that its default model matches the user's request.
Install a missing harness through existing package or platform access when that
installation is authorized. An explicitly supplied key applies to the next
invocation, including when the daemon inherited an older value. Horde records
file priority for that variable, so no restart is needed. Running invocations
retain their credentials.

For Tuara, use `provider_login` action `start` with provider `tuara` or the existing
Tuara provider name and a stable `request_id`. Poll `status`, relay the key-page
URL, and ask the user to sign in and create or copy an inference API key. Submit
the key as `input` with the session ID, then poll until completion. Horde verifies
the key's `router:invoke` access before saving it for the next invocation. Failed
verification, cancellation, or expiry preserves the existing key. This flow needs
no Tuara CLI and makes no inference request; Tuara account OAuth tokens do not
grant inference access. A successful session verifies the key but leaves quota
unknown. Preserve the existing model and role assignments.

For Codex or Claude subscription accounts, use `provider_login` action `start`
with `provider` and a stable `request_id`. The optional `timeout_seconds` defaults
to 600 and accepts 1 through 1800. Poll `status` with the returned `session_id`.
Relay the CLI's URL and device code to the user, who completes browser sign-in.
If the CLI asks for a manual authorization code, ask only for that input and send
it with action `submit`, `session_id`, and `input`. Continue checking status;
use `cancel` if the user abandons sign-in. The user needs no terminal, and the
login process makes no model request.

Reuse the request ID after a lost reply. Sessions survive tool calls and client
disconnects, but a daemon restart requires a new login. This changes the shared
CLI account on the runtime; it does not create a separate subscription profile.
Successful login verifies the CLI's authentication status, while quota remains
unknown. A changed API key or verified login invalidates affected provider quota
observations and preserves local budgets. Existing tasks retain their saved
provider selection; changing role assignments affects new tasks. Reconcile
uncertain work before resuming it.

For a fleet, use `configure_controller`, then `create_fleet_key`. The returned
`bootstrap` describes the daemon argv, environment, and private secret mount for
the calling agent to install through its platform tools. Keep the credential in
the returned private file or the platform's secret facility. On an existing target,
use `join_worker` with `invitation_file` and a useful `name`; new containers or
sandboxes can start the returned daemon argv with `HORDE_ENROLLMENT_FILE` mounted.
Use the same enrollment flow for E2B, Daytona, Docker, Kubernetes, VMs, or single
machines. Each worker needs its own persistent identity directory. SSH is optional
existing execution access, not an enrollment requirement. The setup tool does not
create remote infrastructure; use the caller's authorized platform access.

Check readiness from evidence. `configure_provider` means settings were saved;
`credential:present` does not prove the provider accepts that credential. Use the
login session's authentication result or execute returned authentication-status
checks and report their actual result.
`verify` currently performs inspection: its `not_probed` checks remain unverified.
A configured controller is not a confirmed listener, and successful enrollment is
not proof that a worker can run its selected model. Start the requested daemon,
confirm the authenticated worker connection, and use `runtime_capabilities` to
resolve advertised choices. Report any remaining API or model check honestly.

For “think locally with Codex and Astra, then deliver on Apollo using Claude,
Codex, or GLM 5.3,” configure only the missing requested capabilities. Do not run
every model, create paid workers, or change network policy merely to fill a pool.

Persistent project guidance has a separate approval boundary. Use `skill_inspect`
and present the `skill_propose` diff; call `skill_apply` only after explicit
acceptance. Provider setup or worker enrollment does not authorize skill edits.
