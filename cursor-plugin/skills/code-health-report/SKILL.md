---
name: code-health-report
description: Produce a code-health scorecard for the repo or a directory — composite health signal, inequality, complexity, duplication, documentation gaps, risk-weighted test gaps, and structural hotspots. Use for "code health report", "tech-debt audit", "what's the worst code", complexity/duplication review, or tracking quality across a work session.
---

# Code-health report

Quality-scorecard companion to `tokensave:architecture-overview` (which maps structure; for a module map or layering questions go there). This skill is the **canonical home of the `tokensave_health` drill-down ladder**: lead with the one composite signal, then drill only into the weak dimensions and the specific scans the user asked for — don't run every tool by reflex.

## Workflow

1. **Composite signal → `tokensave_health`** (`details: true`, optional `path`): the 0–10000 score plus the 5-dimension breakdown (acyclicity, depth, equality, redundancy, modularity) and the `coverage_discipline` penalty. The weak dimensions choose the drill-downs.
2. **Inequality / god files → `tokensave_gini`** (`metric`: `complexity`|`lines`|`fan_in`|`fan_out`|`members`, `scope`: `file`|`symbol`, `path?`, `limit?`).
3. **Complexity & size offenders:** `tokensave_complexity` (`limit?`, `node_kind?`, `path?`), `tokensave_largest` (`node_kind?`, `path?`), `tokensave_god_class` (`path?`), `tokensave_hotspots` (`limit?`).
4. **Structure drill-downs (match the weak `health` dimension):** acyclicity → `tokensave_circular` (`max_depth?`) + `tokensave_recursion`; modularity → `tokensave_dsm` (`format`: `stats`|`clusters`|`matrix`) + `tokensave_coupling` (`direction`: `fan_in`|`fan_out`); depth → `tokensave_dependency_depth` + `tokensave_inheritance_depth`; relationships → `tokensave_rank` (`edge_kind` required); kind mix → `tokensave_distribution`.
5. **Duplication → `tokensave_redundancy`** (`min_lines?`, `similarity_threshold?`, `include_naming_only?`, `max_pairs?`, `path?`); near-duplicate names → `tokensave_similar` (`symbol`).
6. **Documentation gaps → `tokensave_doc_coverage`** (`path?`, `limit?`).
7. **Safety / panic sites → `tokensave_unsafe_patterns`** (`kinds?`, `exclude_tests?`, `path?`).
8. **Risk-weighted test gaps → `tokensave_test_risk`** (`path?`, `limit?`): where the next test should go.
9. **Changed-files-only pass (optional) → `tokensave_simplify_scan`** (`files`).
10. **Track a session (optional):** bracket the work via `tokensave:tracking-session-health` for the per-dimension before/after delta.

## Guardrails

- Discovery/analysis tools are read-only and parallel-safe. `tokensave_session_start` / `tokensave_session_end` write/remove `.tokensave/session_baseline.json`; use them only when a before/after delta is relevant and respect Cursor approval/run-mode.
- `tokensave_redundancy` is computed lazily and cached; the first call on a fresh index can be slow on large repos — keep `path`/`max_pairs` tight.
- This skill reports and prioritizes; it does not edit. To fix findings, hand off to `tokensave:atomic-code-edits` / `tokensave:cleaning-up-dead-code`; to verify, `tokensave:running-impacted-tests`. For a focused ship-readiness sweep (panic sites, risk markers, dead code, untested high-risk symbols) use `tokensave:auditing-code-safety` instead of the full scorecard.

## Output

- The composite score + weak dimensions, ranked worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list. Pairs with the `docs-canvas` plugin (if installed) for a rendered scorecard.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
