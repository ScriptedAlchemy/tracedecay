# ADR-006: Evidence Workbench and Renderer Contracts

## Status
Accepted for V2 Phase 0.

## Context
V1 dashboard plugins duplicate state, endpoint semantics, visual vocabularies, and project-local views.

## Decision
The default route is profile-wide All/Brain. One URL-addressable `InvestigationStateV1` coordinates scope, time, query, selection, compare, composition, renderer, and inspector. Atlas, Trace, Compare, Lab, and Triage are bounded compositions over generated client data. Projects are filtered views, not separate products.

Every graph, timeline, table, matrix, chart, inspector, export, and accessibility outline consumes one generated visual-semantic ontology and `VisualizationEnvelopeV1`/frame contract. Evidence class, confidence, provenance, freshness, privacy, exact/sampled/hidden counts, denominator, coverage, watermarks, and layout versions remain visible. Color has redundant shape/text/pattern encoding. Every visualization has table/outline/export parity, keyboard/touch behavior, mobile layouts, reduced motion, and deterministic snapshots.

Renderers are replaceable registered SPIs with typed input, budgets, cancellation, deterministic layout/version, accessibility/export fallback, and no semantic filtering or second selection store. Large graph choice remains a measured bakeoff; no renderer dependency is frozen by aesthetics alone. Labs reuse one hermetic experiment operation and have zero production effects.

## Rejected alternatives
- Independent plugin apps, card mosaics, a universal force graph, 3D/particle decoration, or color-only evidence.
- UI joins of raw endpoints, browser-derived canonical counts, or dashboard-local configuration defaults.
- Renderer-local query/filter/selection truth and bespoke lab runners.

## Compatibility, rollback, and removal gates
Old and new shells coexist behind a flag until deep links, reload, back/forward, mobile, mutation, accessibility, export, and perceptual/comprehension parity pass. Each plugin retains behavior until its owning V2 workspace passes action parity. Legacy routes redirect only during migration and are deleted with the V1 shell after rollback closes.