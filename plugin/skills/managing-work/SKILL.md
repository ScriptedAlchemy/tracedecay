---
name: managing-work
description: 'Create or control TraceDecay Work proposals, attempts, graph mutations, placements, or adjudication. Named Workflow definitions and runs are separate.'
---

# Managing Work

Work is the daemon-owned task/attempt surface. Proposal acceptance precedes
execution admission; an existing attempt's status and evidence are distinct from
starting, retrying, resuming, or canceling it. Consume exact proposal, attempt,
placement, and run identities returned by the preceding operation.

Graph mutation and duplicate adjudication have preparation operations. Preserve
the prepared identity and expected state through application rather than deriving
a new request from a display label. Placement preflight, admission, status, and
release similarly belong to one returned authority.

For multi-root Work queries, use a saved scope set with `multi_root_execute`.
Do not silently substitute the active project's scope. Use live metadata for
available controls and their arguments. Named Workflow lifecycle belongs to
`managing-workflows`; session recovery is read-only history, not Work execution.
