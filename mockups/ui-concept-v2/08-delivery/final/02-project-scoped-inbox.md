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

## Truth boundary

Cross-project membership is never inferred from display proximity alone. Every related item exposes its project, ref, provider source, recency, and correlation class.
