---
name: tracedecay-draft-commit
description: Draft a commit message, PR description, or changelog from semantic changes; drafts text only and never commits or pushes.
---

# /tracedecay-draft-commit

Use `tracedecay:reviewing-changes`.

- **Args:** interpret `$ARGUMENTS` as the target (e.g. "pr", "changelog", a base ref, or "staged"); if absent, draft a commit message for the working tree/staged changes.
- Draft text only — leave `git commit` / `gh pr create` to the user unless they explicitly ask.

Output: the drafted commit / PR / changelog text.
