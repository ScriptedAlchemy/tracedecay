---
design_status: current
evidence_class: concept_synthetic
---

# Exact review coverage, diff, threads, and checks

## User job

Inspect surprising code and determine what is unresolved, risky, weakly evidenced, contradictory, or unreviewed.

## Product behavior

- Exact diff hunks, review threads, check matrix, annotations, and coverage are primary evidence, not hidden behind the story graph.
- Filters focus unresolved reviews, `test_risk`, `unsafe_patterns`, weak-evidence density, contradictions, and unreviewed semantic areas. The UI does not display an unexplained composite risk score.
- Selecting a hunk pivots to the originating episode and retained source; selecting an episode highlights the produced hunks, symbols, and tests.
- Local TraceDecay feedback can attach to exact code or a visible decision artifact. Provider actions remain read-only unless separately authorized.

## Accessibility

All visual coverage and status encodings have table, text, keyboard, and non-color equivalents.

At 200% zoom, the exact diff receives a focus mode while review threads, check evidence, and filters reflow into independently reachable regions. Reduced motion removes animated cross-highlighting and graph travel without removing selection, provenance, or coverage meaning. The exact diff, review-thread table, check matrix, and coverage table remain available as fallbacks.

## Truth boundary

Provider review and check actions remain read-only unless a separately authorized production write path exists. Local TraceDecay feedback is a distinct local artifact and never masquerades as a GitHub comment or review disposition. Visible summaries are not private reasoning.

## Production authorities

- Local Git owns exact refs, commits, files, and diff hunks; Code owns changed symbols and semantic relationships.
- Review and CI/check projections own provider threads, dispositions, check results, and freshness independently.
- `test_risk`, `unsafe_patterns`, weak-evidence links, contradictions, and unresolved or unreviewed counts are the only named review-attention inputs; no generic numeric score is authority.
- Sessions, Work, and Agents provide source-linked episode provenance; local feedback storage owns TraceDecay-only comments and review marks.
