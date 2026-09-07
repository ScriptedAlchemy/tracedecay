---
name: investigating-unexpected-changes
description: 'Attribute unexpected working-tree, branch, or commit changes using Git evidence and correlated sessions while preserving other agents’ work.'
---

# Investigating unexpected changes

Git status, diff, log, and reflog establish what changed in the live checkout.
The indexed graph may describe a different generation; use it to understand
changed symbols, not as the sole authority for current Git history.

Correlate the exact branch, worktree, or commit with session evidence. A session
that produced a commit differs from one that merely observed it; preserve the
reported relation, including unknown legacy attribution. Use matching spans and
messages before claiming an author or intention.

Broad session grep may cap the sessions searched. Scope follow-up replay to the
returned session and time window, retain its anchors, and inspect coverage before
claiming the record is complete. Missing attribution is uncertainty, not evidence
that a peer's change can be overwritten.

When changes overlap active work, resolve ownership and intent before editing.
For retrieving the underlying conversation, use `managing-session-context`.
