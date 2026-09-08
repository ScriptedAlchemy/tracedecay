---
description: Find and safely remove dead code, unused imports, and duplication via the TraceDecay code graph.
argument-hint: "[path]"
---

# Clean dead code

Find and safely remove dead code across the whole repo, or `$ARGUMENTS` if a directory was given.

1. Discover with `tracedecay_dead_code` / `tracedecay_unused_imports` / `tracedecay_redundancy`; focused pass → `tracedecay_simplify_scan` (`files`). Use `tracedecay_unmounted_files` for build-mount evidence, but verify runtime loaders and external consumers before treating a file as dead.
2. Before deleting anything, confirm zero real callers with `tracedecay_callers` / `tracedecay_rename_preview`. Be conservative with `pub` items (they may be used outside the indexed scope). Never delete a symbol whose callers/references are non-empty.
3. Apply edits via the anchored primitives (`tracedecay_str_replace`, `tracedecay_multi_str_replace`, `tracedecay_replace_symbol`); verify with the native build/typecheck and relevant tests. `tracedecay_affected` selects structural candidates; `tracedecay_run_affected_tests` executes the supported Rust selection. Retained `tracedecay_diagnostics` does not recompile edits. Use `tracedecay_health_delta` when a generation-bound health comparison is useful.

Output: removed/consolidated items and the before/after health or test result.
