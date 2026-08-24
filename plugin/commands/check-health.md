---
description: Check code health for the repo or a directory, including worst offenders and a prioritized fix list.
argument-hint: "[path]"
---

# Check health

Produce a read-only code-health scorecard for the whole repo, or `$ARGUMENTS` if a directory was given. Lead with the one composite signal, then drill only into the weak dimensions — don't run every tool by reflex.

1. Composite signal → `tracedecay_health` (`details: true`, optional `path`): the 0–10000 score plus the 5-dimension breakdown (acyclicity, depth, equality, redundancy, modularity) and the `coverage_discipline` penalty. The weak dimensions choose the drill-downs.
2. Inequality / god files → `tracedecay_gini` (`metric`, `scope`, optional `path`).
3. Complexity & size offenders: `tracedecay_complexity`, `tracedecay_largest`, `tracedecay_god_class`, `tracedecay_hotspots`.
4. Structure drill-downs matched to the weak dimension: acyclicity → `tracedecay_circular` + `tracedecay_recursion`; modularity → `tracedecay_dsm` + `tracedecay_coupling`; depth → `tracedecay_dependency_depth` + `tracedecay_inheritance_depth`.
5. Duplication → `tracedecay_redundancy`; doc gaps → `tracedecay_doc_coverage`; panic sites → `tracedecay_unsafe_patterns`; test gaps → `tracedecay_test_risk`; files the compiler never parses → `tracedecay_unmounted_files`.

This reports and prioritizes; it does not edit.

Output: the composite score + weak dimensions, the worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list. If any result includes a `tracedecay_metrics:` line, report the savings.
