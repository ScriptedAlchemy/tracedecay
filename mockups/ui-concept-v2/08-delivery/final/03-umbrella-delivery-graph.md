---
design_status: current
evidence_class: concept_synthetic
---

# Umbrella Delivery graph

## User job

Understand how multiple repository-specific PRs combine into one product outcome and decide which branch of the outcome needs review next.

## Product behavior

- The umbrella outcome is the selected root; every PR remains separately drillable.
- Rails encode repository ownership, temporal order, status, review coverage, named attention sources, and blocking relationships. Attention may come from `test_risk`, `unsafe_patterns`, weak evidence, contradictions, or unresolved/unreviewed counts; no generic numeric risk score is authoritative.
- Selecting a PR preserves the umbrella breadcrumb and opens that PR's journey.
- The view distinguishes required, optional, inferred, blocked, superseded, and shipped relationships.
- Every membership edge exposes both its correlation basis and grade: shared Work task, session-Git correlation, shared agent, or cross-repository handoff token; `exact`, `inferred`, or `ambiguous`.

## Truth boundary

An umbrella Delivery is a correlation projection, not a provider object. Inferred grouping is labeled and reversible; exact Git/PR/CI evidence remains independently inspectable.

## Access gates

- Keyboard navigation traverses the umbrella root, repository rails, PR nodes, blockers, and preserved breadcrumb in deterministic order.
- Reduced motion removes path drawing and animated handoff or delivery travel while retaining edge type, direction, and status.
- At 200% zoom, the graph can enter a focus view and supporting panels reflow without hiding individually drillable PRs.
- An exact tree/table fallback lists every PR, repository, dependency, correlation basis and grade, review source, and provider outcome.

## Production authorities

- Work owns planned tasks and outcome/task relationships.
- Sessions plus the session-Git correlation index own evidenced session, branch, worktree, commit, and PR joins.
- Agents owns stable agent/subagent attribution; persisted handoff tokens own explicit cross-repository transfers.
- Local Git and provider PR, review, CI, and release projections remain the exact evidence behind each individually drillable PR.
- The umbrella correlation projection may combine these reads but cannot promote an inferred or ambiguous edge to exact.
