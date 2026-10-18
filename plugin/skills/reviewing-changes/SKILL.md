---
name: reviewing-changes
description: Use when the user asks to review, summarize, or assess the risk of a PR, branch, or working-tree diff. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Reviewing changes

Follow these steps in order. Report a step you skip as `skip: <reason>`.

1. Read the actual diff.
2. Load its semantic context: `tracedecay_diff_context` for working-tree
   changes, `tracedecay_pr_context` for a ref-to-ref range. Changed symbols,
   dependents, and test links supplement the diff; they cannot prove unindexed
   or external consumers safe.
3. Deepen on the riskiest changed symbols with callers, impact, and test links
   rather than repeating every analysis across every file. skip: the diff is
   small enough to read whole.
4. Confirm each candidate finding against concrete behavior and a reachable
   caller. A scan hit is a lead: test unwraps and unsafe blocks are not defects
   by themselves. For dead-code or ship-risk work, see
   [safety audit](references/safety-audit.md).
5. Explain each finding against the changed behavior and its evidence.

A review grants no write authority. Before a Git apply, a native integration,
or a worktree cleanup, read [write authority](references/write-authority.md).
Structural test selection belongs to `assessing-impact`; structural mutation
belongs to `editing-safely`.

**Report:** actionable findings only, each with location, failure mode, and
evidence, then the coverage the semantic context lacked. Do not post, resolve,
or apply anything the review did not authorize.
