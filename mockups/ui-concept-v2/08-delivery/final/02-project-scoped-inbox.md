---
design_status: current
evidence_class: concept_synthetic
---

# Project-scoped PR inbox

## User job

Review one project's PR queue without losing agent-touched or causally related PRs in other repositories.

## Product behavior

- Project scope updates the URL and every query authority.
- The primary list contains project PRs; a clearly separated related rail contains cross-project PRs touched by selected agents or correlated to the same outcome.
- Scope, provider freshness, and correlation basis remain visible.
- Switching project returns a deterministic scoped view rather than silently reading the active repository.
- A project with no provider authority or no served PRs retains its local repository field and pipeline. `not_published`, unauthorized, denied, rate-limited, stale, unavailable, and served-empty provider states never erase local Git evidence.
- Review-attention filters name their source (`test_risk`, `unsafe_patterns`, weak evidence, contradictions, unresolved reviews, or unreviewed changes) rather than displaying a generic score.

## Truth boundary

Cross-project membership is never inferred from display proximity alone. Every related item exposes its project, ref, provider source, recency, and correlation class.

## Access gates

- Keyboard navigation reaches project scope, filters, primary and related PR lists, inspector actions, and the local repository pipeline.
- Reduced motion removes animated cross-project transfers and scope transitions without removing grouping or correlation status.
- At 200% zoom, primary and related rails reflow into named regions with readable provider-state explanations and controls.
- An exact table fallback lists every scoped and related PR with project, ref, source, freshness, correlation basis, and destination.

## Production authorities

- URL project scope and the project registry select the exact project/store identity; no active-repository fallback may answer a scoped read.
- The repository field and eight-projection pipeline remain the local per-project authority before or alongside provider data.
- Pull-request, review, CI, release, and freshness projections retain independent typed states and authorization.
- Cross-project related items require a named basis: shared Work task, session-Git correlation, shared agent identity, or cross-repository handoff token, labeled `exact`, `inferred`, or `ambiguous`.
