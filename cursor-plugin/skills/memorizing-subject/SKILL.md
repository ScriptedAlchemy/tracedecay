---
name: memorizing-subject
description: Research a user-specified subject with parallel read-only agents, dedupe the findings, and persist durable facts via tokensave_fact_store. Use only when the user explicitly asks to memorize or remember a subject.
disable-model-invocation: true
---

# Memorizing a subject

An explicit, user-triggered workflow (via the `/memorize-subject` command) that researches a subject with parallel read-only agents and stores only durable, cited facts in tokensave memory. This is an expensive, memory-writing workflow: run it only when the user explicitly asks to memorize/remember a subject. The parent agent is the sole writer.

## Inputs

- **subject = `$ARGUMENTS`** — the topic, code area, branch, PR, or scope to memorize. The subject is the scope boundary for all research and every stored fact.
- If the subject is missing or ambiguous, ask the user to name it **before** doing any research.

## Safety rules

- Research subagents are **READ-ONLY**. They MUST NOT call `tokensave_fact_store` with `action: "add"`, `"update"`, or `"remove"`, and MUST NOT call `tokensave_fact_feedback`. Only the parent agent writes to memory.
- Never store secrets, credentials, tokens, API keys, or PII.
- Never store large code blobs. Prefer citations (file/symbol, branch, PR, doc, or transcript) over copying code.
- Store a `code_area` fact only when it is durable — not transient branch state.
- Keep every fact scoped to the subject.

## Research fan-out (parallel read-only subagents)

Dispatch these angles in parallel; each one only gathers and returns candidate facts, never writes:

1. **Code graph** — start with `tokensave_context` (semantic), then `tokensave_search`, `tokensave_body`, `tokensave_outline`, `tokensave_callers`, `tokensave_callees`, and `tokensave_impact`.
2. **Docs / README** — READMEs, design docs, and module-level documentation for the subject.
3. **History / session** — `tokensave_message_search` over ingested transcripts, plus `tokensave_fact_store` with `action: "search"` to see what memory already holds.
4. **Branch / PR** — the branch or PR context relevant to the subject.
5. **Architecture / risk** — structure, dependencies, and risks tied to the subject.

Each candidate fact reports: `content`, a `category` (one of `project`, `general`, `code_area`, `decision`, `tool`, `user_pref`), `entities`, `tags`, a confidence level, citations, and a short rationale.

## Parent synthesis

- Merge and dedupe the candidates; reject anything transient, uncited, low-confidence, secret, oversized, or out-of-scope.

## Trust calibration (tiered)

Map confidence to a `trust` score — and **avoid defaulting everything to high trust; aim for a spread** that reflects real confidence. A store where every fact is 0.9 carries no signal:

- **≥ 0.85 (high)** — verified, durable facts: confirmed decisions, behavior you observed directly, explicit user statements, citations that you re-checked.
- **~ 0.7 (medium)** — ordinary well-sourced observations that were not independently verified.
- **~ 0.5 (low)** — plausible but unverified; usually **do not store without user approval** — prefer not storing over storing noise.

Trust is recall ranking input, so inflated trust pollutes future retrieval for every agent that follows.

## Dedupe before writing

- For each surviving fact, search first: `tokensave_fact_store` with `action: "search"`, `query` (subject + the fact), the candidate's `category`, `limit: 10`, and `min_trust: 0.5`. (Recall memory first in general — before any external or web search, prior sessions often already answered the question.)
- Skip near-duplicates that already exist.
- If a stored fact is close-but-stale or contradictory, report it for user approval — do not overwrite it.

## Read the add result's diff report

Every `action: "add"` result includes additive fields `diff`, `closest_fact_id`, `similarity`, `reason`:

- `near_duplicate` — a very similar fact already exists; prefer `action: "update"` on `closest_fact_id` (or accept the dedupe) instead of piling on duplicates.
- `possible_conflict` — a negation/state-change cue ("no longer", "switched from", "instead of", "replaced", "deprecated") suggests supersession; confirm which fact is current and report the pair for user review.
- `rejected_secret_like` — the content matched a credential pattern and was **not stored**; never rephrase to sneak it past the filter.

## Store accepted facts

For each accepted, non-duplicate fact, call `tokensave_fact_store` with `action: "add"` and:

- `content` — the fact.
- `category` — one of `project`, `general`, `code_area`, `decision`, `tool`, `user_pref`.
- `source` — `"memorize-subject"`.
- `tags` — `["memorize-subject", "<subject-slug>"]`.
- `entities` — the relevant entity names.
- `trust` — from the confidence mapping above.
- `metadata` — `{ "subject": ..., "confidence": ..., "research_angle": ..., "citations": ... }`.

## Feedback

- Do **not** call `tokensave_fact_feedback` during storage. It records `helpful` / `unhelpful` on a fact that was actually used later (adjusting its trust), not at write time.

## Output

- **Stored** facts (id, category, content).
- **Skipped** duplicates.
- **Rejected** candidates, with the reason.
- **Uncertain** candidates that need user approval before storing.
- If any tool result includes a `tokensave_metrics:` line, report the savings to the user.
