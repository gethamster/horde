# Service contracts

Inspect both ends of the relevant interaction before describing it. Use the
actual handler, caller, request/response types, and configured routing. A type
name or endpoint declaration alone may omit serialization or forwarding behavior.

## Describe the actual interface

Capture the fields relevant to the change in the existing contract format:

- The behavior owner, callers, protocol, and exact method or operation name.
- The route or endpoint as seen by its intended caller, including relevant prefixes.
- Authentication mechanism, credential audience, authorization scope, and actor identity.
- Request and response shapes, required fields, defaults, units, and validation.
- Error responses, partial results, and what happens when a dependency is unavailable.
- Retry, timeout, cancellation, idempotency, and uncertain-outcome behavior.
- Streaming, event ordering, reconnect, or version semantics where applicable.
- References to the owning source, configuration, and relevant compatibility version.

Follow forwarded requests through the relevant boundaries. A browser-facing
route, application proxy, and private service route can have different paths,
credentials, and failure behavior. Do not present a proxy as the owner of the
underlying state or assume it forwards every field unchanged.

## Match the audience

Use the existing public specification for interfaces intended for external
consumers, and the existing internal documentation for service and operational
contracts. Do not decide visibility from a route prefix alone. A publicly
reachable endpoint can still require narrow authorization, and an internal
interface still needs an accurate contract.

Document caller-visible URLs, credential types, and examples for that audience.
Use placeholders or safe synthetic values, never live secrets or private payloads.
Do not copy internal-only endpoints or configuration into public documentation
merely because they appear in the source tree.

## Preserve compatibility

A documentation cleanup does not authorize renaming a wire field, route, schema
column, event, or public type. Treat an authorized contract change as compatibility
work: inspect callers, update the owning definitions and relevant examples, and
follow the repository’s versioning or migration process.

If implementation and intended contract disagree, identify the difference and
its evidence. Do not quietly redefine the contract to hide a bug, or alter the
implementation as an incidental documentation edit. Keep the correction within
the user’s authorized scope.

Where generated schemas exist, change their owning source and regenerate affected
outputs using the established commands. Where they do not, update the existing
reference format rather than introducing OpenAPI or a contract registry by default.
