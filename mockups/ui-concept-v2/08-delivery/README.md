# Delivery concept

Delivery is TraceDecay's local-first environment for understanding what is ready to ship and, when provider authority is configured, discovering, reviewing, and delivering the growing volume of agent-generated pull requests. It explains how changes came to exist without replacing exact diffs, transcripts, review threads, checks, Git history, or provider evidence.

Route: `/delivery`.

## Authoritative set

The reviewed state sequence and per-image product briefs are indexed in [final/README.md](final/README.md). Files at this directory level are historical iterations and are not implementation authority.

The final sequence covers the provider-enabled PR-review wing:

1. Global PR discovery across every ingested project.
2. Project-scoped PRs plus agent-touched and correlated cross-project work.
3. Umbrella Deliveries connecting individually drillable PRs across repositories.
4. A horizontal causal journey from human objective to delivered outcome.
5. Temporal replay and agent/subagent branching.
6. Honest partial, missing, stale, inferred, and unavailable evidence.
7. Exact review coverage, diffs, threads, and checks.
8. A full-size Follow the Story review workspace.
9. A real-evidence Decision-to-Code reconstruction for PR #743.

## Local-first entry and coexistence

The PR-review sequence does **not** replace the shipping Delivery page. With no usable provider authority, `/delivery` opens the Repositories wing: the measured repository field plus the eight independently typed Delivery projections. It remains the home for repositories with no PRs, non-Git project bodies, dirty working trees, local commit and branch history, release evidence, and generation-freshness questions.

The repository field preserves indexed recency, body size, branch count, checkout count, dirty-working-tree state, and the explicit unknown/non-Git band. Selecting a repository opens the existing projection pipeline:

1. working-tree changes;
2. commits;
3. pull requests;
4. reviews;
5. CI and checks;
6. failure localization;
7. releases; and
8. generation freshness.

Each projection keeps its own ready, served-empty, stale, rate-limited, denied, not-published, unavailable, or failed state. One healthy local projection never paints a provider projection green.

When `github_read_authority` is configured and the pull-request projection is usable, the global or project PR inbox becomes an upgrade from that local-first landing—not a prerequisite for using Delivery. If authority is missing, the inbox shows `not_published · requires github_read_authority` and a truthful configuration affordance. Unauthorized, denied, rate-limited, stale, unavailable, and served-empty responses remain distinct; none collapse into a blank list or fabricated zero.

## Product boundary

- A Delivery can be one PR or an umbrella outcome spanning correlated PRs.
- Work's task graph is compared with observed sessions, agent work, code, checks, and outcomes.
- Provider/GitHub lifecycle and evidence are read-only in this concept unless a real authorized write path is implemented.
- Local TraceDecay comments and feedback attach to visible persisted artifacts; the product never claims private chain-of-thought.
- Exact, inferred, ambiguous, stale, missing, and unavailable correlations remain distinct.
- Review attention is source-named. The UI may expose `test_risk`, `unsafe_patterns`, weak-evidence density, contradictions, and unresolved or unreviewed counts; it must not invent a generic numeric PR risk score.

## Evidence and correlation language

Delivery uses one evidence-grade ladder across every state:

- `exact`: directly joined to a stable local, repository, transcript, Git, or provider identity;
- `explicit`: a persisted human or agent claim, summary, decision, or rationale with its source;
- `inferred`: a computed relationship with its basis shown;
- `ambiguous`: multiple plausible identities or relationships remain;
- `unavailable`: the authority, artifact, or private reasoning is not available.

Retained rationale, observed repository facts, and check results are source classes, not alternate confidence grades. No decimal confidence score implies precision the authorities do not supply.

Umbrella Deliveries may correlate PRs through shared Work tasks, the session-Git correlation index, shared agent identities, or cross-repository handoff tokens. Every edge names its basis and reports `exact`, `inferred`, or `ambiguous`; display proximity alone never creates membership.

## Historical provenance

Superseded and rejected Delivery iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for the repository-field and independent-authority studies.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language.
- [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns shared state coverage.
- Production Delivery projections remain independently typed: local changes, commits, pull requests, review evidence, CI checks, failure localization, releases, and generation freshness.
- The Repositories wing preserves the shipping repository field and `DeliveryOverviewV1` pipeline as the pre-provider and per-repository authority.
- Provider PR, review, check, and release reads remain independently authorized and read-only. Missing `github_read_authority` is a typed product state, not an empty inbox.
- Journey, umbrella, and Decision-to-Code views require an honest projection over Work, Sessions, Agents, local Git, code index, and provider evidence. Exact, explicit, inferred, ambiguous, stale, missing, and unavailable links survive the join.
