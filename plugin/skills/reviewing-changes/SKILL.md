---
name: reviewing-changes
description: 'Review semantic risk in a PR, branch, or working-tree diff, including changed callers, coverage gaps, and unsafe or redundant code.'
---

# Reviewing changes

Read the actual diff and use `diff_context` for working-tree changes or
`pr_context` for ref-to-ref semantic context. Their changed symbols, dependents,
and test links supplement the diff; they cannot prove unindexed or external
consumers safe. Deepen on a risky symbol rather than repeating every analysis
across all changed files.

A scan hit is a lead: test unwraps, unsafe blocks, and vector-only duplicate
matches are not automatically defects. Confirm concrete behavior and reachable
callers. For dead-code or ship-risk work, see [safety audit](references/safety-audit.md).

Generation-bound Git reads do not authorize index/ref writes. TraceDecay Git
preview/apply (`tracedecay_git_preview`, then `tracedecay_git_apply`) consumes
exact returned authority. Feedback advisory handles are read-only diagnostics
and never permission to post or resolve GitHub comments. Branch-stack
integration requires its frozen snapshot and approved application identity
(`tracedecay_approve_native_integration`, `tracedecay_apply_native_integration`,
`tracedecay_cancel_native_integration`); an ordinary review does not authorize
integration.

Worktree cleanup consumes inventory, inspection, confirmation, removal
(`tracedecay_worktree_cleanup_remove`), and reconciliation identities and does
not delete a branch. Preserve peer ownership and reconcile typed outcomes rather
than reconstructing a target from its name.

Explain findings against the actual changed behavior and evidence. Structural
test selection belongs to `assessing-impact`; structural mutation belongs to
`editing-safely`.
