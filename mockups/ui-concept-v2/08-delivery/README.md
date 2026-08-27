# Delivery concept

Delivery is TraceDecay's agent-native environment for discovering, understanding, reviewing, and delivering the growing volume of agent-generated pull requests. It explains how changes came to exist without replacing exact diffs, transcripts, review threads, checks, Git history, or provider evidence.

Route: `/delivery`.

## Authoritative set

The reviewed state sequence and per-image product briefs are indexed in [final/README.md](final/README.md). Files at this directory level are historical iterations and are not implementation authority.

The final sequence covers:

1. Global PR discovery across every ingested project.
2. Project-scoped PRs plus agent-touched and correlated cross-project work.
3. Umbrella Deliveries connecting individually drillable PRs across repositories.
4. A horizontal causal journey from human objective to delivered outcome.
5. Temporal replay and agent/subagent branching.
6. Honest partial, missing, stale, inferred, and unavailable evidence.
7. Exact review coverage, diffs, threads, and checks.
8. A full-size Follow the Story review workspace.

## Product boundary

- A Delivery can be one PR or an umbrella outcome spanning correlated PRs.
- Work's task graph is compared with observed sessions, agent work, code, checks, and outcomes.
- Provider/GitHub lifecycle and evidence are read-only in this concept unless a real authorized write path is implemented.
- Local TraceDecay comments and feedback attach to visible persisted artifacts; the product never claims private chain-of-thought.
- Exact, inferred, ambiguous, stale, missing, and unavailable correlations remain distinct.

## Historical asset ledger

Historical PNGs remain indexed here until the approved replacements and cleanup are independently accepted.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-recency-field.png](v1-recency-field.png) | [v1-recency-field.md](v1-recency-field.md) | `superseded` | Repository recency study; too narrow for the final product. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Historical HUD study. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Historical HUD study. |
| [v3-independent-authorities.png](v3-independent-authorities.png) | [v3-independent-authorities.md](v3-independent-authorities.md) | `superseded` | Honest authority model retained as historical source, but not the final interaction. |

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language.
- [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns shared state coverage.
- Production Delivery projections remain independently typed: local changes, commits, pull requests, review evidence, CI checks, failure localization, releases, and generation freshness.
