---
name: domain-language
description: >-
  Use when naming or editing domain terms in documentation, UI copy, comments,
  routes, types, fields, events, metrics, or configuration. Reuse established
  meanings, define genuinely new concepts, and treat published identifier
  changes as compatibility work rather than prose cleanup.
metadata:
  audience: "contributors"
---

# Domain Language

Use names that reflect the actual domain and the repository’s established
meaning. Inspect the behavior and owning source before naming an unfamiliar
component or operation. This skill concerns terminology and identifiers; it
does not impose one product’s vocabulary on every repository.

## Find the established term

Read the relevant existing glossary, contract, source declaration, documentation,
or product copy. A glossary is useful when one exists; do not require a new
registry to rename a local variable or explain an existing concept.

Use one established term for one meaning. Do not invent a second label or replace
a precise term with a metaphor to make prose sound more impressive. Preserve
meaningful distinctions between concepts that merely have similar names. When
repositories use different terms for the same integration, document the mapping
rather than silently renaming their contracts.

Use [Naming decisions](references/naming-decisions.md) for a new concept,
ambiguous term, or inconsistent prose. Respect the codebase’s naming conventions
and deliberate product names; use role names instead of person-specific
instructions when writing shared procedures.

## Introduce new concepts deliberately

Check whether the new thing is already an instance of a known concept. If it
needs a distinct name, choose a plain term from the relevant domain and define
it briefly in the existing owning documentation or contract. State what makes
it different and link the source that owns its behavior where useful.

Keep the term consistent across the changed interface, prose, examples, and
implementation. Do not force unrelated repositories or documents into a global
rename. A name shared with another concept needs disambiguation, not an assumption
that the two concepts are interchangeable.

## Separate prose edits from compatibility changes

Improve unclear wording within the requested document or code scope. Preserve
external proper names, quotations, and source attribution where those names are
facts rather than project terminology. Do not turn a terminology correction into
an unrequested rewrite of the repository.

A route, serialized field, schema column, published event, public type, metric,
or configuration key may have consumers outside the edited file. Follow
[Compatibility-sensitive renames](references/compatibility-renames.md) before
changing one. Use existing authorization for an approved rename; request a
scope decision only when the compatibility work exceeds that authorization.
Do not ask again merely because the already-authorized change has a migration.

## Verify the meaning remains clear

Check references and examples affected by the change. Verify that old and new
terms do not falsely imply different objects or collapse different states.
For identifier changes, verify the relevant consumers and compatibility behavior
through the repository’s required process. Report unresolved ambiguity or
migration limits rather than claiming a prose edit completed the migration.

The [adaptation record](references/source.md) identifies the original guidance.
