---
design_status: current
evidence_class: concept_synthetic
---

# Global PR inbox

## User job

After provider read authority is configured, find every ingested pull request across all projects, identify unresolved or weakly evidenced work, and recognize PRs that jointly produce one umbrella outcome.

## Product behavior

- Provider-enabled Delivery entry with no project selected. The local repository field remains the default when the provider projection is not usable.
- Filters by project, status, named review-attention source, coverage, agent, and unresolved evidence. Valid sources include `test_risk`, `unsafe_patterns`, weak evidence, contradictions, and unresolved or unreviewed counts; the product has no generic numeric PR risk score.
- The PR list remains primary and accessible; the graph groups correlated PRs without hiding individual entries.
- Selecting a PR opens its journey. Selecting an umbrella opens the multi-repository outcome graph.
- `not_published · requires github_read_authority`, unauthorized, denied, rate-limited, stale, unavailable, and served-empty inbox states remain visibly distinct. A failed provider read never becomes a healthy empty list.

## Truth boundary

The plate is synthetic. Production correlation must label exact, inferred, ambiguous, stale, missing, and unavailable links separately. Provider controls are read-only unless a real authorized write path exists.

## Access gates

- Keyboard navigation reaches filters, the PR list, umbrella groups, the inspector, and the local-first fallback without requiring graph interaction.
- Reduced motion removes animated graph travel and transitions while preserving selection, grouping, status, and correlation meaning.
- At 200% zoom, filters and the inspector reflow around the PR list; no label, state explanation, or action is clipped or hover-only.
- An exact table fallback exposes every PR, project, provider state, correlation basis, count, and selectable destination represented by the graph.

## Production authorities

- The project registry and shipping repository field own the local-first fallback and repository selection.
- The independently typed pull-request projection owns provider publication, authorization, freshness, pagination, and served-empty state; it requires `github_read_authority` when GitHub data is not published locally.
- Review and CI projections provide unresolved review counts and check evidence independently of PR-list readiness.
- Umbrella edges are based only on shared Work tasks, session-Git correlation, shared agents, or cross-repository handoff tokens, with each edge labeled `exact`, `inferred`, or `ambiguous`.
