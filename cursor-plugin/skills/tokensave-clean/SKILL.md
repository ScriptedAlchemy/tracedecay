---
name: tokensave-clean
description: Find and safely remove dead code, unused imports, and duplication via the tokensave code graph.
disable-model-invocation: true
---

# /tokensave-clean

Apply the `tokensave:cleaning-up-dead-code` skill.

- **Scope:** the whole repo, or the directory named after the command if one was given.
- Follow that skill's workflow and guardrails: confirm zero real callers before deleting anything, be conservative with `pub` items, and respect Cursor approval/run-mode for edits and verification runs.

Output: removed/consolidated items and the before/after health or test result.
