---
name: drafting-commit-and-pr
description: Use when drafting a commit message, PR description, release notes, or changelog from semantic diff context; drafts text only and must not commit, push, or create PRs.
---

# Drafting commit & PR text

## Workflow

1. **Commit message → `tracedecay_commit_context`** (`staged_only`): changed symbols + file roles + recent commit style → draft a message that matches the repo's style.
2. **PR description → `tracedecay_pr_context`** (`base_ref`, `head_ref`): semantic summary → draft body (Summary / Impact / Tests).
3. **Release notes → `tracedecay_changelog`** (`from_ref`, `to_ref`): categorized added / removed / modified symbols.
4. **Sanity-check what actually changed → `tracedecay_branch_diff`** (base vs head graph).

## Guardrails

- Read-only with respect to the working tree: this skill drafts text only. Leave `git commit` / `gh pr create` to the user or a dedicated git workflow.

## Output

- The drafted commit / PR / changelog text.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
