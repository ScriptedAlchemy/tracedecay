---
name: assessing-impact
description: 'Use when estimating blast radius, finding what depends on a symbol or file, choosing or running affected tests, or verifying a change without a full suite. Trigger before guessing tests or running "cargo test" broadly. Do NOT use to review a diff (tracedecay:reviewing-changes).'
---

# Assessing impact

```
NO GUESSED TEST LISTS AND NO FULL-SUITE-BY-DEFAULT.
Compute the dependent set and the affected tests from the graph first.
```

Announce: "Using tracedecay:assessing-impact for <change>."

## Blast radius

1. Have changed file paths already? → `tracedecay_diff_context` (`files`):
   modified symbols + dependents + affected tests in ONE offline call — prefer
   this over separate lookups. Works with no network and no PR.
2. Single symbol → resolve to node ID (`tracedecay:exploring-code` ladder),
   then `tracedecay_impact` (`node_id`, shallow `max_depth` first).
3. File-level fan-in → `tracedecay_file_dependents`.
4. Optional fragility check → `tracedecay_coupling` / `tracedecay_dependency_depth`.

## Coverage intelligence (read-only)

| Question | Call |
|---|---|
| Which tests reach this symbol/file | `tracedecay_test_map` (empty result = no indexed path, strong-not-absolute evidence of untested) |
| Which tests can see these changed files | `tracedecay_affected` (`files`) |
| Where the next test is most needed | `tracedecay_test_risk` (`path?`, `limit?`) |
| "What breaks if I change/touch this" | `tracedecay_impact` (`node_id`) for a symbol, `tracedecay_affected` (`files`) for changed files |
| Prior run outcomes for those tests | `tracedecay_test_results` |

## Running the impacted tests

1. `tracedecay_run_affected_tests` (`changed_paths`, `max_tests`, `profile`,
   `timeout_secs`): pass/fail per test with covered source nodes. **Cargo/Rust
   only** — for other stacks use `tracedecay_affected` to get the test set,
   then the project's own runner on exactly that set. Persist or re-read outcomes with
   `tracedecay_test_results` when comparing reruns.
2. Compile/type failures during the run → `tracedecay:fixing-build-and-type-errors`.

## Rules

- Everything except `run_affected_tests`/`diagnostics` is read-only — preview
  scope freely before running toolchains; respect host approval/run-mode and
  avoid duplicate toolchain runs (first diagnostics build can take minutes).
- Coverage is structural: integration tests reaching code through binaries,
  fixtures, or IO can be missed. Say so when asserting "untested".
- Refactor checklist wanted → `tracedecay:editing-safely`. Whole-diff review →
  `tracedecay:reviewing-changes`.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_diff_context,tracedecay_impact,tracedecay_affected,tracedecay_test_map,tracedecay_run_affected_tests`.
- MCP error: `tracedecay tool diff_context --files …` etc. (see
  `tracedecay:using-the-cli`). Do not degrade to running the full suite.

## Deliverable

Do not end without all three: (a) impacted symbols + files, (b) the concrete
test set (or pass/fail summary mapped back to source), (c) coverage gaps or
hub risk worth flagging. If the graph cannot see part of the change (e.g.
generated code), state that explicitly instead of widening to a full suite.
Report any `tracedecay_metrics:` line.
