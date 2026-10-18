# Work operations

Each operation consumes the exact proposal, attempt, placement, or run identity
the preceding operation returned, never one derived from a display label. Use
live metadata for available controls and their arguments.

- **Tasks and proposals.** `tracedecay_work_create` creates a task. Proposal
  review and acceptance (`tracedecay_work_review_proposal`,
  `tracedecay_work_accept_proposal`) precede execution admission
  (`tracedecay_work_admit_execution`).
- **Attempts.** An attempt's status and evidence are distinct from starting,
  retrying, resuming, or canceling it (`tracedecay_work_start_attempt`,
  `tracedecay_work_retry_attempt`, `tracedecay_work_resume_attempts`,
  `tracedecay_work_cancel_attempt`).
- **Runs.** A run pauses or resumes as a whole (`tracedecay_work_pause_run`,
  `tracedecay_work_resume_run`).
- **Graph mutation and adjudication.** Preparation operations return a prepared
  identity and expected state that `tracedecay_work_mutate_graph`,
  `tracedecay_work_adjudicate_duplicate`, and `tracedecay_work_adjudicate_leak`
  must receive unchanged.
- **Placement.** Preflight, admission (`tracedecay_work_admit_placement`),
  status, and release (`tracedecay_work_release_placement`) belong to one
  returned authority.
- **Synthesis.** `tracedecay_work_synthesize` admits one synthesis attempt over
  exact sibling attempt identities. An unsynthesized answer is a typed refusal,
  not a failed attempt.
- **Multi-root queries.** Use a saved scope set with `multi_root_execute`. Do
  not silently substitute the active project's scope.
