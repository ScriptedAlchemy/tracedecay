---
design_status: current
evidence_class: concept_synthetic
---

# Expanded agent branches

## User job

Follow a parent agent, its subagents, their tasks and worktrees, and the evidence returned through handoffs.

## Product behavior

- Spawn, nested spawn, handoff, result, failure, and rejoin are distinct edge types.
- The selected workstream expands while unrelated work compresses into context bundles.
- Each branch exposes agent identity, task, session, worktree, commits, edits, tests, and result evidence.
- Path to root and path to outcome isolate causally relevant branches.

## Scale

Overview never allocates one permanent lane per agent. Large fan-out uses task/workstream clustering, edge bundling, deterministic placement, lazy evidence loading, and a virtualized branch navigator.

## Truth boundary

Spatial proximity does not imply collaboration. Spawn, handoff, result, and rejoin links require exact persisted identities or remain labeled inferred or ambiguous. Private chain-of-thought is unavailable and is never represented by a branch.

## Access gates

- Keyboard navigation traverses the virtualized branch navigator, bundles, path-to-root or path-to-outcome controls, and selected source evidence.
- Reduced motion removes branch expansion travel and animated handoffs while preserving topology, edge type, result, and failure state.
- At 200% zoom, the selected branch receives a focused workspace and contextual bundles reflow rather than shrinking agent or event labels.
- Exact tree, table, transcript, worktree, commit, diff, test, and result fallbacks preserve every represented branch relationship.

## Production authorities

- Agents owns agent/subagent identities, parentage, spawn, and result records; Sessions owns transcript and event provenance.
- Work owns tasks and workstream clustering; local Git owns worktrees, branches, commits, and changed files.
- Cross-repository branch bundles use explicit handoff tokens or named session-Git/shared-agent correlations, preserving `exact`, `inferred`, and `ambiguous` status.
- Exact tree/table fallbacks remain authoritative when the visual aggregation is compressed.
