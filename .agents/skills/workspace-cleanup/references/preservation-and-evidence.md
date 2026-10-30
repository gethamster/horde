# Preservation and evidence

Classify each resource before deletion. Git tracking, file age, or apparent build
location does not determine whether its contents are valuable.

| Material | Required treatment |
| --- | --- |
| Modified or staged source | Preserve in the retained candidate, a verified commit, or a supported recoverable archive. |
| Untracked source and task outputs | Inspect and preserve needed files; a commit alone may not include them. |
| Unique commits | Verify integration or retain reachable commits before removing their checkout or references. |
| Ignored files | Distinguish reproducible build output from required artifacts, private configuration, databases, and evidence. |
| Release or qualification artifacts | Preserve required immutable outputs and their source/build identity in the established durable location. |
| Application state, credentials, backups, or upstream ledgers | Keep them outside routine cleanup; use the owning lifecycle policy for any separately authorized removal. |
| Investigation or validation evidence | Preserve the evidence needed to explain the result or continue unresolved work, respecting its access restrictions. |
| Reproducible output no remaining work needs | Remove only after confirming ownership, lack of active consumers, and that it contains no protected material. |

Do not preserve secrets or private runtime data by committing them. Use the
existing authorized private storage and access controls. Keep a permitted durable
reference in the task report instead of copying private content into a public or
broader-audience record.

## Verify the preservation destination

The destination must survive removal of the source. An archive inside the
worktree being removed is not preservation. Check that the retained version or
archive can be read and contains the required source, untracked files, and
artifacts. Account for formats or tools that omit ignored files.

For integrated work, verify the retained candidate includes the actual intended
changes. Squash merges and cherry-picks can preserve changes without preserving
commit ancestry. Conversely, a merged branch does not prove that uncommitted or
untracked work was retained.

For evidence, preserve enough identity to connect it to the tested source,
artifact, environment, and outcome. Keep required original evidence and backups
according to their owning policy; a summary is not automatically a replacement.
Do not delete an unresolved failure’s evidence just because a newer attempt passed.

## Keep the report useful

Record the resource identity, owner, preservation destination where needed,
confirmed retirement, and reason for any retention. Keep the report in the
existing task or project record rather than inventing another tracking system.

A retained resource should have a concrete reason, such as active testing,
continuing worker use, required handoff, or protected state. When the reason is
uncertain, name the unanswered question and retain the item. Do not silently
classify unknown content as disposable to claim completion or recover more space.
