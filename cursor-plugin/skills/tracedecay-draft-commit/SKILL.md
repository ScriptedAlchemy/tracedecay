---
name: tracedecay-draft-commit
description: Draft a commit message, PR description, or changelog from the semantic meaning of the changes (drafts text only — never commits or pushes).
disable-model-invocation: true
---

# /tracedecay-draft-commit

Apply the `tracedecay:drafting-commit-and-pr` skill.

- **Args:** interpret the text after the command as the target (e.g. "pr", "changelog", a base ref, or "staged"); if absent, draft a commit message for the working tree/staged changes.
- Follow that skill's guardrails: it drafts text only — leave `git commit` / `gh pr create` to the user unless they explicitly ask.

Output: the drafted commit / PR / changelog text.
