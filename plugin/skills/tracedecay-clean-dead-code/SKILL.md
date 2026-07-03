---
name: tracedecay-clean-dead-code
description: 'Use to find and safely remove dead code, unused imports, and duplication via the TraceDecay code graph.'
---

# Clean dead code

Use when asked to find and safely remove dead code, unused imports, or duplication.

Route this through the `tracedecay:reviewing-changes` skill to identify candidates, then apply `tracedecay:editing-safely` for any removals.

- **Scope:** the whole repo, or a specific directory if one is named.
- Follow those skills' guardrails: confirm zero real callers before deleting anything, be conservative with `pub` items, and verify with a build/test re-check after edits.

Output: removed/consolidated items and the before/after health or test result.
