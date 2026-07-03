---
name: tracedecay-clean-dead-code
description: 'Use to find and safely remove dead code, unused imports, and duplication via the TraceDecay code graph.'
---

# Clean dead code

Use to find and safely remove dead code, unused imports, or duplication.

Use `tracedecay:reviewing-changes` to identify candidates, then `tracedecay:editing-safely` for removals.

- **Scope:** the whole repo, or a specific directory if one is named.
- Confirm zero real callers before deleting anything; be conservative with `pub` items; verify with a build/test re-check after edits.

Output: removed/consolidated items and the before/after health or test result.
