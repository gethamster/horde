# Naming decisions

Start with what the thing does and who owns its meaning. Prefer a term a reader
can trace to a behavior, record, or contract over an appealing but undefined label.

## Reuse before inventing

Search the relevant source declarations and current documentation for the concept.
Use the existing glossary if available, but do not treat an old or unrelated
definition as more authoritative than the current owning contract. If the sources
conflict, identify the conflict instead of choosing a convenient meaning silently.

Distinguish the concept from its implementation. A product module, running
service, process, repository, and deployment artifact are different things even
when their names are similar. Contributor tools and runtime agents also serve
different audiences. Preserve those distinctions in summaries and diagrams.

Do not assume that a familiar word has identical semantics across services.
A job, request, task, and operation may have different lifecycles and identities.
Explain the mapping at the integration boundary rather than using the terms
interchangeably without evidence.

## Name a genuinely new concept

Choose a plain domain term that fits the behavior and avoids collision with an
existing concept. Follow the repository’s conventions for casing, units, type
names, and identifiers. Do not introduce abbreviations an intended reader cannot
resolve, or use a metaphor when an established technical term already says it.

For a shared concept, add a short definition in the existing owning contract,
glossary, or documentation. Include the distinction that matters and a source
reference when useful. Do not create a new glossary or rename a family of
unrelated types solely to satisfy this skill.

## Edit prose within scope

Fix ambiguous references and inconsistent terminology where the requested change
touches them. Keep definitions and examples aligned. Preserve historical product
names in source attribution and historical records when replacing them would
falsify the record; make current names clear in current-state documentation.

Use the relevant role in reusable instructions, such as user, operator,
maintainer, or reviewer. Name an actual person only when that identity is a
necessary fact of the record, not as a hardcoded recipient of routine instructions.
