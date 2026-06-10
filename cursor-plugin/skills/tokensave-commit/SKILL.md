---
name: tokensave-commit
description: Draft a commit message, PR description, or changelog from the semantic meaning of the changes.
disable-model-invocation: true
---

# /tokensave-commit

Apply the `tokensave:drafting-commit-and-pr` skill.

- **Args:** interpret the text after the command as the target (e.g. "pr", "changelog", a base ref, or "staged"); if absent, draft a commit message for the working tree/staged changes.
- Follow that skill's guardrails: it drafts text only — leave `git commit` / `gh pr create` to the user unless they explicitly ask.

Output: the drafted commit / PR / changelog text.
