---
name: tracedecay-clean-dead-code
description: Find and safely remove dead code, unused imports, and duplication via the TraceDecay code graph.
---

# /tracedecay-clean-dead-code

Use `tracedecay:reviewing-changes` to identify candidates, then `tracedecay:editing-safely` for removals.

- **Scope:** the whole repo, or the directory named in `$ARGUMENTS` if one was given.
- Confirm zero real callers before deleting anything; be conservative with `pub` items; respect Cursor approval/run-mode for edits and verification runs.

Output: removed/consolidated items and the before/after health or test result.
