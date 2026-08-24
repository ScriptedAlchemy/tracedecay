# Safety audit & dead-code cleanup

Ship-readiness sweep and delete-safely protocol for `tracedecay:reviewing-changes`.

- Panic/unsafe, TODO, dead-code, and untested-risk discovery
- Ranking ship-blockers
- Zero-caller confirmation before deleting

## Safety audit (ship-readiness sweep)

1. **Panic & unsafe sites → `tracedecay_unsafe_patterns`** (`kinds?` to
   narrow to `unwrap`/`unsafe`, `exclude_tests: true` for production-only,
   `path?`): each hit carries file, line, kind, enclosing symbol, `in_test`.
2. **Unfinished work → `tracedecay_todos`** (`kinds:
   ["FIXME","HACK","XXX","UNIMPLEMENTED"]`).
3. **Unreachable code → `tracedecay_dead_code`** (`include_public: true` for
   workspace-internal audits), **`tracedecay_unused_imports`**, and
   **`tracedecay_unmounted_files`** (files no build root reaches — no
   compiler, bundler, or test runner ever loads them, so nothing in them was
   type-checked or linted, however healthy the graph makes them look).
4. **Risky and untested → `tracedecay_test_risk`**: high-complexity,
   high-fan-in symbols with weak coverage.
5. **Rank:** production panic/unsafe in hot paths first (cross-check fan-in
   with `tracedecay_callers`), then UNIMPLEMENTED/HACK markers, then untested
   high-risk symbols, then dead code and imports.

## Dead-code cleanup

1. Discover with `tracedecay_dead_code` / `tracedecay_unused_imports` /
   `tracedecay_redundancy`; focused pass → `tracedecay_simplify_scan` (`files`).
   Start with `tracedecay_unmounted_files`: a file nothing reaches is dead
   wholesale, and every symbol in it looks live to the other three tools.
2. **Before deleting anything → confirm zero real callers** with
   `tracedecay_callers` / `tracedecay_rename_preview`. Be conservative with
   `pub` items (they may be used outside the indexed scope). Never delete a
   symbol whose callers/references are non-empty.
3. Apply edits via `tracedecay:editing-safely`; verify with
   `tracedecay_diagnostics` and the affected tests
   (`tracedecay:assessing-impact`). Optionally bracket the cleanup with the
   session-health delta in `tracedecay:code-health`.
