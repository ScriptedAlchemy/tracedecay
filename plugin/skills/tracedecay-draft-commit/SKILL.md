---
name: tracedecay-draft-commit
description: 'Use to draft a commit message, PR description, or changelog from semantic changes; drafts text only and never commits or pushes.'
---

# Draft commit

Use when asked to draft a commit message, PR description, or changelog from the current semantic changes.

Route this through the `tracedecay:reviewing-changes` skill to read the diff and its impact.

- **Target:** the artifact to draft (e.g. "pr", "changelog", a base ref, or "staged"). If none is given, draft a commit message for the working-tree/staged changes.
- Follow that skill's guardrails: this drafts text only — leave `git commit` / `gh pr create` to the user unless they explicitly ask.

Output: the drafted commit / PR / changelog text.
