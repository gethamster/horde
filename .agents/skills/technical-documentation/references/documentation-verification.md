# Documentation verification

Choose verification for the document’s claims and the files actually changed.
Follow the repository’s required checks without inventing a new documentation
framework or running unrelated application suites for a prose edit.

## Use existing source and generation tools

Locate the authoritative input for generated schemas, reference pages, diagrams,
and navigation. Edit that input and run the established generator when affected.
Inspect the output diff for unintended contract or layout changes. Preserve
required generated files and provenance together, using the repository’s policy.

Do not change a generated view by hand to make it disagree with its source.
If the generator itself is changed, run its relevant tests and verify a real
output. If the repository has no generator for a document, maintain the existing
authored file rather than creating a generation pipeline by default.

## Check the claims

- Resolve relative links, headings, referenced files, and navigation entries.
- Compare exact operation names, routes, field names, defaults, units, and examples
  with the owning implementation and configuration.
- Check the intended audience and remove accidental disclosure of secret values,
  private payloads, or internal-only details from public output.
- Verify code or command examples where safe and authorized; mark unexecuted
  examples honestly rather than implying operational qualification.
- Check rendered diagrams or documentation layouts when their appearance changed.
- For a behavior change, use the owning runtime or contract tests that exercise
  the affected boundary; a generated document alone is not behavioral proof.

## Keep the publication claim accurate

A file saved in a working tree, a merged documentation change, and a published
site are different outcomes. Follow the user’s requested endpoint and the owning
repository’s publication process. Do not publish a private document merely to
make a link available or expand a local-only request into deployment.

Report which checks passed, which outputs were regenerated, and what remains
unverified. Distinguish source-inspected behavior, configured topology, tested
integration, and observed deployment. Do not label a conceptual architecture
picture or a mocked test as evidence of a complete running system.
