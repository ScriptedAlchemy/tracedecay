---
design_status: current
evidence_class: concept_synthetic
---

# Local-first provider not configured

## User job

Understand what Delivery can show before GitHub authority is configured, why the pull-request inbox is unavailable, and how to continue with local repository evidence.

## Product behavior

- Delivery opens on the Repositories wing when provider read authority is absent.
- The measured repository field and accessible table retain last-indexed recency, branch count, checkout count, working-tree changes, body size, ahead or behind, local commits, and index freshness.
- Last-indexed recency is explicitly not commit time. Non-Git directories remain in an `unknown` band; unknown branch state is never fabricated as zero.
- The eight independent projections render local changes and commits, pull requests, reviews, CI, failure localization, releases, and generation freshness with their own typed states.
- The PR inbox shows `not_published · requires github_read_authority`, not a successful empty list.
- `served_empty`, rate-limited, denied, stale retained, unavailable, and `not_published` remain distinguishable inbox outcomes.
- `Open Settings · Provider authority` navigates to configuration. `Continue with local evidence` stays in the Repositories wing; neither control fabricates provider success.

## Truth boundary

The plate is synthetic and represents product states, not the operator's current registry. Local Git readiness does not imply provider readiness. Failure localization remains `not_configured` until an ingestion owner exists. Provider/GitHub writes remain unavailable.

## Production authorities

- The project registry owns repository, checkout, worktree, Git or non-Git identity, and active scope.
- Local Git owns working-tree changes, commits, branches, ahead or behind, and exact refs.
- The code index owns last-indexed time and generation freshness; it never substitutes index time for commit time.
- Delivery's PR, review, CI, release, and failure-localization projections own independent typed provider states such as unauthorized, `not_published`, rate-limited, denied, stale, unavailable, and served empty.
- Settings owns provider authority configuration. The Delivery plate only navigates there.

## Access and scale gates

The field has an exact table fallback and keyboard navigation. State does not rely on color. Reduced motion removes field transitions and animated status changes while preserving repository position, projection state, and the provider explanation. At 200% zoom the repository table and projection list reflow or scroll within named regions without hiding the inbox explanation or configuration path.
