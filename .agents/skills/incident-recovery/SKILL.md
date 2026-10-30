---
name: incident-recovery
description: >-
  Use when an unresolved incident needs cross-service diagnosis or a repair is
  blocked by uncertain committed effects, partial streams, or conflicting state.
  Reconcile the specific failure and return to delivery; do not use for an ordinary
  bounded bug with a known reproduction and straightforward fix.
metadata:
  audience: "contributors"
---

# Incident Recovery

Resolve the concrete uncertainty preventing a fix or safe continuation. This is
an exception path, not a standing prerequisite for every change. Start with the
failed operation and its existing evidence; do not open a broader audit.

Use the [incident evidence map](references/incident-evidence.md) to connect a
failure to relevant services, runtime identity, source, and artifact. Follow
only the boundaries needed to establish the cause. Preserve private evidence
in its authorized location rather than copying it into wider reports.

Use [Recovery and concurrency](references/recovery.md) when an operation may
already have committed, a stream is partially delivered, or overlapping work
has left ownership or state uncertain. Reconcile the original operation identity
before replaying an effect. A timeout alone does not prove nothing happened.

Choose a cause supported by evidence, make the bounded repair, and use a focused
check to prove the affected operation. Keep independent work moving. If required
evidence or access is unavailable, name that specific blocker; do not invent a
new monitoring system, indefinite investigation, or unrelated cleanup project.

Once the cause or operation state is established, return to
[verified-delivery](../verified-delivery/SKILL.md) to smoke-test, decide, and ship.
Additional observation is warranted only by the incident’s concrete risk or an
existing requirement. Stop when that question is answered.

The [source record](references/source.md) records this split.
