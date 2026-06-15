---
name: memorizing-subject
description: Use when the user explicitly asks to memorize or remember a subject, code area, PR, branch, or decision set by researching it and storing only durable cited facts.
disable-model-invocation: true
---

# Memorizing a subject

Expensive memory-writing workflow for `/memorize-subject`. Run only on an explicit memorize/remember request. For curation, updates, deletes, or stale-fact cleanup, use `tracedecay:curating-project-memory`.

## Inputs

- **subject = `$ARGUMENTS`** — topic, code area, branch, PR, or scope boundary. If missing or ambiguous, ask before research.

## Workflow

1. **Research read-only.** Gather candidates from code graph (`tracedecay_context`, `tracedecay_search`, `tracedecay_body`, `tracedecay_outline`, callers/callees/impact), docs, `tracedecay_message_search`, existing `tracedecay_fact_store` search results, and relevant branch/PR context. Research agents, if used, never write memory.
2. **Filter.** Keep only durable, scoped, cited facts. Reject secrets, credentials, PII, large code blobs, transient branch state, uncited speculation, and unsupported claims.
3. **Calibrate trust.** Use ~0.85+ for independently verified decisions/observations, ~0.7 for ordinary well-sourced facts, and ~0.5 for plausible but uncertain facts. Store low-trust facts normally at the requested trust; hygiene, trust scoring, holographic similarity, and curator/audit workflows prune or repair them over time. Do not ask for approval solely because trust is low.
4. **Dedupe before writing.** Search `tracedecay_fact_store` with the subject + candidate, matching category, `limit: 10`, and `min_trust: 0.5`; skip near-duplicates and ask before replacing contradictory facts.
5. **Store accepted facts.** Call `tracedecay_fact_store` `action: "add"` with content, category, source `"memorize-subject"`, tags (`"memorize-subject"`, subject slug), entities, trust, and metadata containing subject/confidence/citations.
6. **Read add diffs.** Act on `near_duplicate`, `possible_conflict`, and `rejected_secret_like`; never rephrase a rejected secret to bypass the filter.

## Guardrails

- Parent agent is the only writer. Do not call `tracedecay_fact_feedback` during storage; feedback is for later helpful/unhelpful ratings on facts that were actually used.
- Prefer citations over copied code. Store `code_area` facts only when they describe durable behavior or ownership, not in-progress branch state.

## Output

- Stored facts (id/category/content/trust), skipped duplicates, rejected candidates with reasons, and any low-trust facts stored for later curation.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
