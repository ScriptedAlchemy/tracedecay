---
name: code-health-report
description: Use when producing a code-health scorecard, tech-debt audit, worst-code ranking, complexity/duplication/doc-gap/test-risk report, or before/after health review; use architecture-overview for module maps.
---

# Code-health report

Quality-scorecard companion to `tracedecay:architecture-overview` (which maps structure; for a module map or layering questions go there). This skill is the **canonical home of the `tracedecay_health` drill-down ladder**: lead with the one composite signal, then drill only into the weak dimensions and the specific scans the user asked for — don't run every tool by reflex.

## Workflow

1. **Composite signal → `tracedecay_health`** (`details: true`, optional `path`): the 0–10000 score plus the 5-dimension breakdown (acyclicity, depth, equality, redundancy, modularity) and the `coverage_discipline` penalty. The weak dimensions choose the drill-downs.
2. **Inequality / god files → `tracedecay_gini`** (`metric`: `complexity`|`lines`|`fan_in`|`fan_out`|`members`, `scope`: `file`|`symbol`, `path?`, `limit?`).
3. **Complexity & size offenders:** `tracedecay_complexity` (`limit?`, `node_kind?`, `path?`), `tracedecay_largest` (`node_kind?`, `path?`), `tracedecay_god_class` (`path?`), `tracedecay_hotspots` (`limit?`).
4. **Structure drill-downs (match the weak `health` dimension):** acyclicity → `tracedecay_circular` (`max_depth?`) + `tracedecay_recursion`; modularity → `tracedecay_dsm` (`format`: `stats`|`clusters`|`matrix`) + `tracedecay_coupling` (`direction`: `fan_in`|`fan_out`); depth → `tracedecay_dependency_depth` + `tracedecay_inheritance_depth`; relationships → `tracedecay_rank` (`edge_kind` required); kind mix → `tracedecay_distribution`.
5. **Duplication → `tracedecay_redundancy`** (`min_lines?`, `similarity_threshold?`, `include_naming_only?`, `max_pairs?`, `path?`); near-duplicate names → `tracedecay_similar` (`symbol`).
6. **Documentation gaps → `tracedecay_doc_coverage`** (`path?`, `limit?`).
7. **Safety / panic sites → `tracedecay_unsafe_patterns`** (`kinds?`, `exclude_tests?`, `path?`).
8. **Risk-weighted test gaps → `tracedecay_test_risk`** (`path?`, `limit?`): where the next test should go.
9. **Changed-files-only pass (optional) → `tracedecay_simplify_scan`** (`files`).
10. **Track a session (optional):** bracket the work via `tracedecay:tracking-session-health` for the per-dimension before/after delta.

## Guardrails

- Discovery/analysis tools are read-only and parallel-safe. `tracedecay_session_start` / `tracedecay_session_end` write/remove `.tracedecay/session_baseline.json`; use them only when a before/after delta is relevant and respect Cursor approval/run-mode.
- `tracedecay_redundancy` is computed lazily and cached; the first call on a fresh index can be slow on large repos — keep `path`/`max_pairs` tight.
- This skill reports and prioritizes; it does not edit. To fix findings, hand off to `tracedecay:atomic-code-edits` / `tracedecay:cleaning-up-dead-code`; to verify, `tracedecay:running-impacted-tests`. For a focused ship-readiness sweep (panic sites, risk markers, dead code, untested high-risk symbols) use `tracedecay:auditing-code-safety` instead of the full scorecard.

## Output

- The composite score + weak dimensions, ranked worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list. Pairs with the `docs-canvas` plugin (if installed) for a rendered scorecard.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
