# Architecture and operations

Describe responsibilities and actual boundaries rather than inferring architecture
from directory names, process counts, or deployment terminology.

## Show ownership and flow

Identify the component that owns each operation and durable record. Distinguish
forwarding adapters, execution services, storage, human decisions, and contributor
tooling. A shared server or repository does not imply shared credentials, data,
authority, or process identity. A conceptual module need not be a container.

Trace the relevant request and response paths, including authentication and
trust changes, retries, streaming, and failure handling when they affect the
claim. State whether data is persisted, transformed, or only forwarded. Do not
invent queues, databases, service roles, or runtime agents to complete a diagram.

Keep conceptual diagrams distinguishable from a complete deployment inventory.
Record which profile, version, or environment a topology describes. A source
configuration, successful local test, and observed production deployment support
different claims; label them accurately.

## Keep current state and decisions distinct

Current-state documentation should explain how the system works now within the
stated evidence. Decision records explain why a choice was made and may describe
an older state. Update the current reference and link the decision history;
do not use an old decision record as the only account of present behavior.

Preserve historical decisions unless the decision itself is being revised.
Annotate superseded assumptions where they would mislead a reader. Remove stale
duplicate explanations within the requested scope instead of adding another
competing architecture description.

## Write operational procedures for the owning environment

Use the repository’s existing runbooks and supported lifecycle commands. State
prerequisites, target environment, required authority, relevant state, expected
result, verification, and recovery steps. Keep credentials and private environment
values out of committed examples.

Separate installation, migration, restart, rollback, and destructive reset. A
procedure that replaces state is not an ordinary restart. Describe relevant
preservation requirements and use the owning tool’s existing recovery mechanism.
Do not assume a native service, sandbox, container, or cloud deployment unless
the documented profile actually uses it.

When code or configuration changes invalidate a command or ownership boundary,
update the affected runbook with the change. Avoid publishing a plausible but
unexecuted command as verified: identify which steps were inspected, tested, or
observed in the target environment.
