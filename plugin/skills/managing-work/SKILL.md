---
name: managing-work
description: Create or control TraceDecay Work tasks, attempts, graph mutations, placements, or adjudication.
---

# Managing Work

Work is the daemon-owned task/attempt surface. `tracedecay_work_create` mints a
task; proposal review and acceptance (`tracedecay_work_review_proposal`,
`tracedecay_work_accept_proposal`) precede execution admission
(`tracedecay_work_admit_execution`). An existing attempt's status and evidence
are distinct from starting, retrying, resuming, or canceling it
(`tracedecay_work_start_attempt`, `tracedecay_work_retry_attempt`,
`tracedecay_work_resume_attempts`, `tracedecay_work_cancel_attempt`), and a run
is paused or resumed as a whole (`tracedecay_work_pause_run`,
`tracedecay_work_resume_run`). Consume exact proposal, attempt, placement, and
run identities returned by the preceding operation.

Graph mutation and duplicate adjudication have preparation operations whose
prepared identity and expected state must be preserved through
`tracedecay_work_mutate_graph`, `tracedecay_work_adjudicate_duplicate`, and
`tracedecay_work_adjudicate_leak`, rather than deriving a new request from a
display label. Placement preflight, admission
(`tracedecay_work_admit_placement`), status, and release
(`tracedecay_work_release_placement`) similarly belong to one returned
authority. `tracedecay_work_synthesize` admits one synthesis attempt
over exact sibling attempt identities; an unsynthesized answer is a typed
refusal, not a failed attempt.

For multi-root Work queries, use a saved scope set with `multi_root_execute`.
Do not silently substitute the active project's scope. Use live metadata for
available controls and their arguments. Named Workflow lifecycle belongs to
`managing-workflows`; session recovery is read-only history, not Work execution.
