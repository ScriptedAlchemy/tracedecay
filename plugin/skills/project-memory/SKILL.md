---
name: project-memory
description: Use when the user asks what was decided, agreed, or recorded about the project, or asks to remember a decision. Skip when the user asks what an earlier conversation said. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Project memory

The canonical fact store is project-wide and survives branch or worktree removal.
Resolve the intended registered project before reading or writing another
project's memory. Use facts for durable knowledge and session history for
transient progress or the exact prior conversation.

Store (`tracedecay_fact_store_add`) within the user's intent, retaining evidence
and calibrated trust. Search for an existing fact before adding a correction;
act on duplicate/conflict results and update (`tracedecay_fact_store_update`)
rather than duplicating. Never store secrets or rephrase a rejected secret to
bypass filtering. Age alone is not evidence that a fact is wrong. Feedback
(`tracedecay_fact_feedback`) should describe whether a recalled fact actually
helped or misled, not reward retrieval by itself.

Supersession preserves an older fact's payload, trust, and provenance by id while
removing it from default retrieval. A fact has one successor; assigning another
is a typed refusal. Removal is permanent. An exact deletion instruction is
sufficient; resolve an ambiguous target before deleting.

Broad curation (`tracedecay_fact_store_curate`) is an agent-managed run, not a
caller-authored batch of arbitrary operations. See
[curation](references/curation.md) for admission, terminal effects, and
verification. Inspectors can supply evidence without gaining write authority.

**Report:** each fact used with its id, trust, and provenance, or the id of the
fact stored or changed. Say when no fact matched; do not fill the gap from this
session's context.
