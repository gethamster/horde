# Upstream maintenance authority

System can bind a nightly upstream review to a durable Horde task without giving
an agent authority to approve its own grant. The binding is optional, so ordinary
tasks retain their existing behavior. System owns the routine, authorization,
upstream cutoff and completion report. Horde owns worker admission, aggregate
worker usage, cancellation and delivery checks.

An operator registers a policy with `maintenance_policy_register {policy}`. Worker
and project-bound credentials cannot register it. The policy contains:

| Field | Meaning |
| --- | --- |
| `schema_version`, `policy_id`, `policy_revision` | Version 1, stable identity and monotonically increasing revision |
| `project_id`, `tenant_id`, `repo` | Existing project/tenant and canonical registered checkout |
| `repository`, `base` | Owning GitHub repository and branch for configured delivery |
| `automation_grant_id`, `routine_id`, `expires_at` | Original authority and its Unix-second expiry |
| `budget` | Approved worker budget: `reference`, `max_model_calls`, `max_input_bytes`, `max_output_bytes` |
| `authority_url`, `authority_token_env` | Exact owning authorization endpoint and daemon credential reference |
| `allow_private_http` | Explicit operator opt-in for an internal HTTP endpoint; otherwise HTTPS or literal loopback HTTP is required |

The RPC is idempotent for an identical policy. Repeating an old registration
cannot undo revocation. Changed authority requires a higher policy revision.
Policies and usage use the existing operator-owned runtime settings; no schema
migration or direct SQLite administration is required.

`submit_task` accepts `upstream_maintenance` with `schema_version: 1`, policy and
revision, `automation_grant_id`, `routine_id`, `occurrence_id`, `invocation_id`,
`collected_until`, `expires_at`, `budget`, and `upstreams`. The last field maps exactly `vercel/ai`,
`vercel/chat`, and `vercel/eve` to frozen 40-character commit IDs. System must derive
this object from its actual admitted occurrence rather than model arguments.
`collected_until` is the original source-freeze Unix-second cutoff, never a retry
time. Horde presents every descendant with the immutable binding, RFC3339 cutoff,
and deterministic `cargo xtask upstream collect --heads-file <file> --until <cutoff>`
instructions. The heads file must contain exactly the binding upstream map.
Horde checks the registered authority and current owning authorization before
creating a task. Existing request receipts retain their normal immutable retry
semantics; reading a receipt does not readmit execution.

## Owning authorization

The daemon sends an authenticated `GET` to the registered `authority_url`, with
`x-upstream-maintenance-invocation` and `x-upstream-maintenance-occurrence` headers.
The service must confirm `active: true` and the exact `policy_id`,
`policy_revision`, `automation_grant_id`, `routine_id`, `invocation_id`,
`occurrence_id`, `collected_until`, `upstreams`, and `expires_at`. Redirects, unavailable authority, changed
identity, expired authority and oversized responses fail closed. Authorization
responses are limited to 16 KiB and the check has a three-second timeout. The
credential stays in the daemon environment and never enters task arguments,
worker prompts or event records.

Horde rechecks the owner before model requests, commands and Git publication,
including the existing GitHub delivery path. In-flight maintenance work is
supervised against the same owner; cancellation uses the existing process-group
cleanup. Local revocation is checked again after an authorization response arrives.
These checks do not make Horde a security sandbox against hostile processes
running as the same operating-system user.

`maintenance_policy_get {policy_id}` inspects only the selected project's policy.
Its `active` field describes the local registration and expiry; each invocation
still requires the online owning check. `maintenance_policy_revoke {policy_id,
policy_revision}` is available to that project and to its operator. It narrows
existing authority, records revocation and cancels the bound root tasks and their
descendants. No callback is needed to revoke during an owner outage. A normal
supervisor completion may leave admitted durable work running until its worker
authority expires; the owning callback must distinguish completion from cancellation.

## Worker budgets

Native model attempts reserve one call and the serialized request bytes before
sending. Response bytes are charged before parsing or dispatching tool output.
Advisory decision requests and retries share those same counters. Usage is keyed
by policy, revision, invocation and budget reference: parallel root batches and
all local descendants cannot reset the allowance by starting a new task.

The limits cover aggregate provider wire bytes and call attempts, including failed
or uncertain attempts. A response can arrive with a chunk larger than the remaining
allowance; Horde rejects it before parsing and closes the response. Attempting to
exceed any limit persists an invocation-wide fence before returning the error, so
command and shipping retries cannot continue after exhaustion. Using the final
admitted allowance does not discard an otherwise valid final response. These limits
are not a dollar or strict token-spend guarantee. Existing provider/account budgets
continue to apply independently.

External CLI harnesses and remote maintenance execution are not qualified for
these aggregate worker budgets and fail closed. Federation cannot silently reroute
a bound task to an unqualified remote runtime. Command and provider failures keep
their normal durable evidence and uncertain effects must be reconciled.

## Delivery and release status

The maintenance binding does not grant human review or acceptance authority.
Configured automatic merge still requires the operator-owned automatic-delivery
policy, its qualification evidence and all normal branch protections. Repository
and base must match the maintenance policy.

An optional, default-off `[automatic_delivery.package_release]` contract supports
only `repository = "gethamster/adam"`, `workflow = "release.yml"`, and `main`.
It requires a maintenance-bound root task on its registered local project and
repository. This contract retains required checks, independent review, approvals,
source checkpoints, qualified decision policy and normal protected merging.
It cannot carry application health assertions or deployment workflow selection.

After protected merge, Horde requires a new stable workspace version and runs
Adam's `xtask check-release-version`. It persists an exact tag/commit/policy intent
before creating the tag. Retries reconcile that intent and never move or adopt an
unowned existing tag. Only the exact tag-push workflow for that commit qualifies;
manual dispatch and main-branch push runs do not. Horde verifies a successful run,
public non-prerelease release, four platform archives and checksum companions,
archive paths and binary architectures, then executes the native binary's
`--version`. The retained receipt pins tag, commit, workflow ID, archive digests
and release URL. Authority is rechecked before publication and completion.

An operator can enroll retained held-out evidence through
`delivery_qualification_enroll {qualification_id,evidence_hash,
reviewed_evidence_hash,approved:true,project?}`. Project-bound and worker credentials
cannot enroll. The report must already exist in the content-addressed store and
satisfy the existing exact provider/model/backend fingerprint, full policy hash,
threshold, at least 50 held-out cases, quality, defect recall and verified completion
latency rules. Failed validation rolls back enrollment. An enrolled identity is
immutable; changed evidence requires a new identity and matching policy.
Synthetic test reports do not qualify a live installation.

Task summaries distinguish implemented package-release support from a verified
release receipt. Configuring this path does not establish held-out qualification,
GitHub credentials, repository protections, or a successful release. Completion
requires the actual published receipt and downstream consumer qualification.
