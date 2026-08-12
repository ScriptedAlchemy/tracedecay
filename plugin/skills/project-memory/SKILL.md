---
name: project-memory
description: 'Use when about to save or recall anything durable: before writing MEMORY.md, auto-memory, or CLAUDE.md, before answering from a stale summary, or before web-searching what a prior session already answered. Covers store, recall, curation. Do NOT use for raw transcript replay. Never stores secrets.'
---

# Project memory

```
DURABLE FACTS LIVE IN THE CANONICAL DURABLE-FACT STORE, NOT IN MEMORY.md OR CLAUDE.md.
RECALL BEFORE RE-DERIVING; STORE WITHOUT BEING ASKED.
```

Announce: "Using tracedecay:project-memory to <recall/store/curate>."

## Route the moment

| Moment | Action |
|---|---|
| Need a prior decision/preference/pitfall | `tracedecay_fact_store_search` (`query`, `min_trust`) — before web search, before asking the user |
| Prior conversations, not facts | `tracedecay_message_search` (`query`, `limit`) — this skill owns FTS→fact; raw replay/scoped grep → `tracedecay:managing-session-context` |
| A durable decision/correction/pitfall just surfaced | `tracedecay_fact_store_add` (`content`, `category`, `tags`, `trust`) — proactively, do NOT wait to be asked, and do NOT write MEMORY.md instead |
| User rates a recalled fact | `tracedecay_fact_feedback` (`helpful`/`unhelpful`) |
| A recalled fact you were shown helped or misled you | `tracedecay_fact_feedback` on its `fact_id` (`helpful`/`unhelpful`) — don't wait to be asked |
| User asks to run or inspect broad memory cleanup | Agent-managed curation flow below |
| User gives an exact fact add/update/remove instruction | Use the matching retained fact command; confirm only an ambiguous destructive target |

Trust calibration for adds: `0.85+` independently verified decisions, `~0.7`
ordinary well-sourced facts, `~0.5` plausible-but-uncertain. The add path
already rejects secrets and reports near-duplicates/conflicts — act on those
flags; never rephrase a rejected secret to bypass filtering.

Rate what you recall: any `fact_id` shown in tracedecay_context's Memory
Matches (or returned by `tracedecay_fact_store_search`) that materially helped or misled
you should get `tracedecay_fact_feedback` (`helpful`/`unhelpful`) the moment you
act on it — proactively, without waiting for the user, since recalled facts are
almost never rated and feedback is how trust is earned.

Do NOT capture: secrets/credentials/PII, transient errors,
environment-specific failures, one-off narratives, task progress, or
soon-stale session outcomes — those belong to session transcripts.

## Curation (agent-managed mutation)

Read [references/curation.md](references/curation.md) for the full protocol:
read-only inventory → `tracedecay_fact_store_curate` (or `tracedecay tool
fact_store_curate`) → `tracedecay_automation_run_list` →
`tracedecay_automation_run_view` → advertised artifact inspection →
read-only fact verification. The curator validates and applies supported
operations during that one run. Its public trigger accepts only review bounds,
never caller-supplied operations or effect authority. Hard rules that always
apply:

- Deletion is permanent (no soft-delete, no undo). A direct fact deletion is
  separate exact administration. If the user's target is ambiguous, show the
  resolved fact id and content summary and confirm that target before removal;
  an exact deletion instruction needs no redundant confirmation.
- Subagents may inspect and recommend only — never let a subagent call
  `tracedecay_fact_store_add`, `tracedecay_fact_store_update`,
  `tracedecay_fact_store_remove`, or `tracedecay_fact_feedback`.
- Do not lower trust merely for age; cite newer evidence or a contradiction.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_fact_store_search,tracedecay_fact_store_add,tracedecay_message_search,tracedecay_fact_feedback,tracedecay_fact_store_curate,tracedecay_automation_run_list,tracedecay_automation_run_view,tracedecay_automation_run_artifact_view`.
- MCP error: `tracedecay tool tracedecay_fact_store_search --query …` (see
  `tracedecay:using-the-cli`). An MCP failure is not a reason to write
  MEMORY.md — the CLI reaches the same store.

## Deliverable

Recall: the prior context/decisions found, with source and trust — or an
explicit "no stored fact matches", after which storing the fresh answer is the
default next step. Store: the fact id(s) written and any duplicate/conflict
flags handled. Curate: run id, terminal status, applied/rejected operations,
advertised artifacts inspected, and verification result. Report any
`tracedecay_metrics:` line.
