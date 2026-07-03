---
description: Draft a commit message, PR description, or changelog from semantic changes; drafts text only and never commits or pushes.
---

# Draft commit

Interpret `$ARGUMENTS` as the target (e.g. "pr", "changelog", a base ref, or "staged"). If absent, draft a commit message for the working-tree/staged changes.

1. Commit message → `tracedecay_commit_context` (`staged_only`): changed symbols + file roles + recent commit style.
2. PR description → `tracedecay_pr_context` (`base_ref`, `head_ref`): Summary / Impact / Tests.
3. Release notes → `tracedecay_changelog` (`from_ref`, `to_ref`); sanity-check with `tracedecay_branch_diff`.

This drafts text only — leave `git commit` / `gh pr create` to the user unless they explicitly ask.

Output: the drafted commit / PR / changelog text. If any result includes a `tracedecay_metrics:` line, report the savings.
