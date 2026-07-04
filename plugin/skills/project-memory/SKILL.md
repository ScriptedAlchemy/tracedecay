---
name: project-memory
description: 'Use when about to save or recall anything durable: before writing MEMORY.md, auto-memory, or CLAUDE.md, before answering from a stale summary, or before web-searching what a prior session already answered. Covers store, recall, curation. Do NOT use for raw transcript replay. Never stores secrets.'
---

# Project memory

```
DURABLE FACTS LIVE IN fact_store, NOT IN MEMORY.md OR CLAUDE.md.
RECALL BEFORE RE-DERIVING; STORE WITHOUT BEING ASKED.
```

Announce: "Using tracedecay:project-memory to <recall/store/curate>."

## Route the moment

| Moment | Action |
|---|---|
| Need a prior decision/preference/pitfall | `tracedecay_fact_store` `action:"search"` (`query`, `min_trust`) — before web search, before asking the user |
| Prior conversations, not facts | `tracedecay_message_search` (`query`, `limit`) — this skill owns FTS→fact; raw replay/scoped grep → `tracedecay:managing-session-context` |
| A durable decision/correction/pitfall just surfaced | `tracedecay_fact_store` `action:"add"` (`content`, `category`, `tags`, `trust`) — proactively, do NOT wait to be asked, and do NOT write MEMORY.md instead |
| User rates a recalled fact | `tracedecay_fact_feedback` (`helpful`/`unhelpful`) |
| User asks to clean/merge/delete memory | Curation flow below |

Trust calibration for adds: `0.85+` independently verified decisions, `~0.7`
ordinary well-sourced facts, `~0.5` plausible-but-uncertain. The add path
already rejects secrets and reports near-duplicates/conflicts — act on those
flags; never rephrase a rejected secret to bypass filtering.

Do NOT capture: secrets/credentials/PII, transient errors,
environment-specific failures, one-off narratives, task progress, or
soon-stale session outcomes — those belong to session transcripts.

## Curation (mutation — parent agent only, approval required)

Read [references/curation.md](references/curation.md) for the full protocol:
read-mostly inventory → native dry-run (`tracedecay memory curate`) →
candidate buckets → narrow apply (`fact_store` add/update/remove) → read-only
verify (`tracedecay_memory_status` for counts/health). Hard rules that always
apply:

- Deletion is permanent (no soft-delete, no undo). Explicit approval
  immediately before every `remove`, hard delete, or merge-loser removal,
  showing fact id, content summary, and reason. Prefer update/merge when
  provenance should survive.
- Subagents may inspect and recommend only — never let a subagent call
  add/update/remove/feedback, apply curation ops, or run memory repair.
- Do not lower trust merely for age; cite newer evidence or a contradiction.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_fact_store,tracedecay_message_search,tracedecay_fact_feedback`.
- MCP error: `tracedecay tool fact_store --action search --query …` (see
  `tracedecay:using-the-cli`). An MCP failure is not a reason to write
  MEMORY.md — the CLI reaches the same store.

## Deliverable

Recall: the prior context/decisions found, with source and trust — or an
explicit "no stored fact matches", after which storing the fresh answer is the
default next step. Store: the fact id(s) written and any duplicate/conflict
flags handled. Curate: facts changed/skipped, approvals obtained,
verification result. Report any `tracedecay_metrics:` line.
