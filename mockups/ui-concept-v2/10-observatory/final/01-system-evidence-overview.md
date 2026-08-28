---
design_status: current
evidence_class: concept_synthetic
---

# Observatory — system evidence overview

- **Asset:** `01-system-evidence-overview.png`
- **Route:** `/observatory`
- **Lifecycle:** authoritative final concept

## User job

Determine which parts of TraceDecay are healthy, degraded, incomplete, stale, denied, or unavailable; find the evidence behind a finding; and decide whether to inspect, recover, or wait without mistaking one healthy subsystem for whole-system health.

## Product behavior

- The canonical-observations timeline is the shared time context. Selecting a point or range filters the independently sourced panels without rewriting their source state.
- Doctor findings, adoption coverage, retrieval quality, code-index stages, hook hints, performance budgets, topology, and storage telemetry report their own status and freshness. There is no decorative global heartbeat.
- Selecting a finding opens the evidence inspector with its named source, observation time, severity, scope, affected objects, details, recovery availability, and last confirmation.
- A recovery control is shown only when a real production recovery route is authorized. It reports queued, running, succeeded, failed, denied, or stale-target states; it never fabricates success.
- Cross-links open the exact source in Code, Sessions, Automations, or Settings while preserving project and time scope in the URL.

## Production authorities

- `ObservatoryPage.tsx` composes `/api/storage/telemetry`, `/api/code-index/freshness`, `/api/observatory`, and analytics diagnostics.
- `DoctorInspector.tsx`, `CanonicalObservations.tsx`, `PerformanceBudgets.tsx`, and `HookHints.tsx` own their corresponding evidence surfaces.
- Daemon observation envelopes own source identity, collection time, freshness, coverage, and typed availability. Store telemetry owns measured capacity and findings. Code-index state owns stage progress. Provider/runtime sources remain independent.
- The shell, project scope, route order, and status colors come from the shared [navigation](../../NAVIGATION.md), [design system](../../DESIGN-SYSTEM.md), and [interaction-state contract](../../INTERACTION-STATES.md).

## Canonical evidence ladder

From strongest to weakest: exact daemon/store/index/provider observation with source and timestamp; exact derived comparison against a named budget; partially covered observation with numerator/denominator; stale last-known observation; explicitly inferred relation; unavailable, denied, omitted, or not-published source. A weaker rung cannot be silently promoted to a stronger one.

`measured`, `partial`, `stale`, `denied`, `unavailable`, `building`, and measured-empty are separate typed states. Missing data is never rendered as zero, nominal, or healthy. A panel may be measured while its neighbor is stale or unavailable.

## Interaction and scale contract

- Keyboard: tab order follows timeline, panel summaries, selected finding, then inspector actions. Arrow keys traverse points/rows; Enter selects; Escape returns to the prior scope. All graph selections have list equivalents.
- Reduced motion: flowing particles and pulse animation become static direction, stage, and timestamp marks; status never depends on animation.
- 200% zoom/reflow: panels stack into one readable column, the inspector becomes a full-width region, and no body text or finding detail is clipped.
- Dense data: virtualize long finding/observation lists, cluster timeline marks by interval, preserve filters, and expose exact counts. Do not render one DOM node per raw observation at overview scale.
- Exact fallback: provide source-qualified observation, finding, budget, topology, and telemetry tables with timestamps, coverage, freshness, and typed state; export/open the underlying evidence where authority permits.

## Truth boundary

This reviewed plate is `CONCEPT / SYNTHETIC DATA`. Its values, dates, findings, topology, and recovery availability are illustrative, not runtime receipts. Production may display them only from the named authorities. Partial, stale, denied, and unavailable states must remain visible even when that makes the screen less visually complete.
