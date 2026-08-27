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
