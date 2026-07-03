---
name: assessing-impact
description: 'Use when estimating blast radius, finding what depends on a symbol or file, choosing or running affected tests, checking whether code is tested, or verifying a change without a full suite. Use before guessing tests, running broad suites, or declaring a change safe.'
---

# Assessing impact

## Blast radius

1. **Resolve the target → node ID** with `tracedecay_search` /
   `tracedecay_find_exact_symbol` / `tracedecay_by_qualified_name` (resolver
   ladder: `tracedecay:exploring-code`).
2. **Symbol blast radius → `tracedecay_impact`** (`node_id`, small `max_depth`
   first, widen only if the picture is incomplete): all direct + transitive
   dependents.
3. **File-level fan-in → `tracedecay_file_dependents`** (every file importing
   the changed file).
4. **Already have changed paths → `tracedecay_diff_context`** (`files`):
   modified symbols + dependents + affected tests in one call — prefer it
   over separate lookups.
5. **Structural fragility (optional):** `tracedecay_coupling` /
   `tracedecay_dependency_depth` to see if the target is a high-fan-in hub.

## Coverage intelligence (read-only)

1. **Symbol/file → its tests → `tracedecay_test_map`** (`file` or `node_id`):
   direct coverage edges; an empty result means no test reaches it through
   the indexed graph.
2. **Changed files → affected tests → `tracedecay_affected`** (`files`):
   dependency-graph traversal to every test file that can see the change.
3. **Where the next test goes → `tracedecay_test_risk`** (`path?`, `limit?`):
   risk = (complexity + 1) × (fan_in + 1) × untested-multiplier — the
   prioritized gap list.

## Running the impacted tests

1. **Run → `tracedecay_run_affected_tests`** (`changed_paths`, `max_tests`,
   `profile`, `timeout_secs`): pass/fail per test, with the source nodes each
   test covers. Cargo-only; for non-Rust repos use `tracedecay_diagnostics`
   (tsc/pyright) and the project's own test runner.
2. **On compile/type failure → `tracedecay_diagnose`** for captured cargo
   stderr, or the `tracedecay:fixing-build-and-type-errors` skill.

## Guardrails

- Everything except `tracedecay_run_affected_tests` and
  `tracedecay_diagnostics` is read-only and safe to run first to preview
  scope. The cargo-backed tools run toolchains (the first `diagnostics` build
  can take minutes; forced target dir
  `/tmp/tracedecay-target/<project_id>/diagnostics`) — respect Cursor
  approval/run-mode and avoid duplicate runs.
- Coverage is structural (call/use edges): integration tests that reach code
  indirectly (through a binary, fixture, or IO boundary) can be missed — an
  empty `test_map` is strong but not absolute evidence of "untested".
- Start with a shallow `max_depth` and widen only when incomplete.
- For broad changes, use scoped read-only subagents per changed file group or
  subsystem; require cited dependents, affected tests, and tool parameters —
  the parent agent owns the final blast-radius and test-set synthesis.

## Handoff

- Mechanical refactor where impact analysis becomes an edit checklist →
  `tracedecay:editing-safely`. Reviewing a whole diff → `tracedecay:reviewing-changes`.

## Output

- (a) impacted symbols + files, (b) the test set to run (or the pass/fail
  summary with failing-symbol mapping), (c) any hub/coupling risk and ranked
  coverage gaps.
- If any result includes a `tracedecay_metrics:` line, report the savings to
  the user.
