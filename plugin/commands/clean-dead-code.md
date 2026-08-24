---
description: Find and safely remove dead code, unused imports, and duplication via the TraceDecay code graph.
argument-hint: "[path]"
---

# Clean dead code

Find and safely remove dead code across the whole repo, or `$ARGUMENTS` if a directory was given.

1. Discover with `tracedecay_dead_code` / `tracedecay_unused_imports` / `tracedecay_redundancy`; focused pass → `tracedecay_simplify_scan` (`files`).
2. Before deleting anything, confirm zero real callers with `tracedecay_callers` / `tracedecay_rename_preview`. Be conservative with `pub` items (they may be used outside the indexed scope). Never delete a symbol whose callers/references are non-empty.
3. Apply edits via the anchored primitives (`tracedecay_str_replace`, `tracedecay_multi_str_replace`, `tracedecay_replace_symbol`); verify with `tracedecay_diagnostics` and the affected tests (`tracedecay_run_affected_tests` / `tracedecay_affected`). Optionally bracket the cleanup with a `tracedecay_session_start` / `tracedecay_session_end` health delta.

Output: removed/consolidated items and the before/after health or test result. If any result includes a `tracedecay_metrics:` line, report the savings.
