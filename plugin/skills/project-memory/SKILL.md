---
name: project-memory
description: 'Recall or maintain durable TraceDecay project facts, decisions, and preferences, or inspect a requested curation run. Use session context for raw transcript replay.'
---

# Project memory

The canonical fact store is project-wide and survives branch or worktree removal.
Resolve the intended registered project before reading or writing another
project's memory. Use facts for durable knowledge and session history for
transient progress or the exact prior conversation.

Store within the user's intent, retaining evidence and calibrated trust. Search
for an existing fact before adding a correction; act on duplicate/conflict
results. Never store secrets or rephrase a rejected secret to bypass filtering.
Age alone is not evidence that a fact is wrong. Feedback should describe whether
a recalled fact actually helped or misled, not reward retrieval by itself.

Supersession preserves an older fact's payload, trust, and provenance by id while
removing it from default retrieval. A fact has one successor; assigning another
is a typed refusal. Removal is permanent. An exact deletion instruction is
sufficient; resolve an ambiguous target before deleting.

Broad curation is an agent-managed run, not a caller-authored batch of arbitrary
operations. See [curation](references/curation.md) for admission, terminal effects,
and verification. Inspectors can supply evidence without gaining write authority.
