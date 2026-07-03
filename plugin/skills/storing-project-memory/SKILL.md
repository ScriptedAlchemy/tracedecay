---
name: storing-project-memory
description: 'Use when writing a durable fact to tracedecay memory — persisting a decision, preference, correction, pitfall, or entity relation, and handling near-duplicate/conflict/secret write diffs. For cleanup see project-memory.'
---

# Storing project memory

This skill owns the **write path** into holographic memory: turning a durable
decision or fact into a stored `tracedecay_fact_store` record. It is the
narrow "add/update/relate" counterpart to `tracedecay:project-memory`
(dedup, merge, delete, whole-subject memorization) and
`tracedecay:retrieving-project-memory` (read/reason). Store proactively
whenever a durable decision, user preference, correction, or pitfall surfaces —
do not wait for the user to ask.

## When to store vs not

Store only **durable, project-scoped** facts:

- Store: architectural/design decisions, user or project preferences, hard-won
  corrections, recurring pitfalls, stable conventions, entity relationships.
- Do NOT store: secrets/credentials/API keys/PII, transient errors,
  environment-specific failures, task progress, one-off narratives, or anything
  that goes stale when the session ends — recover those from transcripts via
  `tracedecay:recalling-session-context` instead.

## Workflow

1. **Dedupe first (read-only):** search before you write with
   `tracedecay_fact_store` `action: "search"` (`query` = subject + candidate,
   optional `category`, `limit: 10`, `min_trust: 0.5`). If a near-match exists,
   prefer an update over a second add.
2. **Add a fact → `tracedecay_fact_store`** `action: "add"` with `content`
   (the durable claim), `category`, `source` (provenance label), `tags`,
   `entities` (named entities the fact concerns), `trust`, and optional
   `metadata` (subject/confidence/citations). The add result carries a
   write-time diff — always read it (see below).
3. **Update an existing fact → `tracedecay_fact_store`** `action: "update"`
   with `fact-id` plus the changed `content`/`trust`/`tags`/`category`. Prefer
   update when correcting or refining a fact so provenance survives.
4. **Relate entities → `tracedecay_fact_store`** `action: "relate"` (with
   `entities` / `entity`) to record a relationship between named entities the
   facts concern.
5. **Calibrate trust deliberately** — do not default high. Aim for a spread:
   `>=0.85` for independently verified/durable decisions, `~0.7` for ordinary
   well-sourced facts, `~0.5` for plausible-but-unsure. Do not lower trust
   merely because a fact is old; cite newer evidence instead.

## Reading the add diff

Every `action: "add"` returns `diff` / `closest_fact_id` / `similarity` /
`reason`. Act on it, never ignore it:

- `near_duplicate` — a very similar fact exists; prefer `action: "update"` on
  `closest_fact_id` rather than storing a second copy.
- `possible_conflict` — a negation/state-change cue suggests supersession;
  confirm which fact is current before leaving both in place (hand off to
  `tracedecay:project-memory` if a merge/delete is needed).
- `rejected_secret_like` — credential-like content was **NOT** stored. Never
  rephrase or obfuscate a rejected secret to bypass the filter.

## Guardrails

- `search` is read-only; `add`, `update`, and `relate` **mutate** memory state.
  `search`/`probe`/`related`/`reason` may update access/retrieval counters.
- Deletion is permanent and lives in `tracedecay:project-memory`, not
  here — prefer update/relate over creating removable clutter.
- Never store secrets, credentials, keys, or PII; rely on the built-in
  `rejected_secret_like` filter as a backstop, not a first line.
- Only the parent agent should call `add`/`update`/`relate`. Subagents may
  gather cited evidence and candidate facts; the parent validates and writes.

## Handoff

- Dedup, merge, delete, or memorize a whole subject → `tracedecay:project-memory`.
- Read, probe, or reason over stored facts → `tracedecay:retrieving-project-memory`.

## Output

- The fact(s) stored/updated with their ids, the trust assigned, and any
  `near_duplicate` / `possible_conflict` / `rejected_secret_like` diff acted on.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
