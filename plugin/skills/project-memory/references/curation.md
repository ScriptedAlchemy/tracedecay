# Memory curation

Full curation and memorize-a-subject protocol for `tracedecay:project-memory`.

- Curate: read-mostly inventory, dry-run, narrow apply, verify
- Curation guardrails (deletion, subagents, dashboard)
- Memorize a subject on explicit request

## Curate (explicit destructive mutation)

Destructive curation is a parent-agent responsibility. Use subagents only for
scoped inspection or recommendation work, with explicit project selectors and
non-overlapping ownership; do not delegate delete/apply/merge/retention actions
to subagents. Begin read-only, gather evidence, propose a mutation plan, then
write only narrow durable changes.

1. **Resolve scope:** confirm the active project root/store before touching
   memory. Project-bound profiles use the user-level TraceDecay store scoped to
   the current project by default.
2. **Start read-only:** `tracedecay_fact_store_get`,
   `tracedecay_fact_store_contradict`, `tracedecay_fact_store_search`,
   `tracedecay_fact_store_list`, `tracedecay_fact_store_probe`,
   `tracedecay_fact_store_related`, or `tracedecay_fact_store_reason`. Search, probe, related, and
   reason preserve derived holographic retrieval and scoring semantics. Use
   `tracedecay_memory_status` only when the user asks for its read-only
   canonical fact/entity/trust/feedback/holographic-algebra status snapshot.
   Use `tracedecay_dashboard` (`action: "start"`) only when they want visual
   curation.
3. **Run native dry-run:** prefer `tracedecay memory curate` or
   `POST /api/plugins/holographic/curate` with `{"dry_run": true}`. Dry-run is
   the default and returns `actions`, `hygiene_candidates`, `counts`,
   `coverage`, `provider`, and `mode`.
4. **Inventory candidates:** group facts into add, update, merge/dedupe, stale,
   contradiction, secret-like, transient, supersession, and possible
   hard-delete buckets. Keep fact ids, source/provenance, trust, tags,
   entities, evidence links, and counterevidence with each candidate.
5. **Research gaps:** use TraceDecay graph/search plus LCM/session/message tools
   to mine past sessions, raw messages, summary DAGs, branch/PR context, docs,
   and tests. Scoped subagents may research bounded read-only questions only;
   the parent agent is the sole memory writer and must review raw findings
   before trusting them.
6. **Propose changes:** summarize durable additions, stale-fact updates,
   trust/tag/source changes, dedupe merges, and delete candidates. Prefer
   update/merge over removal when useful provenance should survive.
7. **Apply narrowly → `tracedecay_fact_store_add`,
   `tracedecay_fact_store_update`, or `tracedecay_fact_store_remove`**
   for reviewed operations (or `POST
   /api/plugins/holographic/curate/apply` / `tracedecay memory curate
   --llm-ops <file> --apply`). Require explicit approval immediately before
   every `remove`, dashboard hard delete, or merge loser removal, showing fact
   id, content/source summary, reason, and permanent-delete warning.
8. **Verify read-only:** re-run search/list/probe/related/contradict/get as
   appropriate, inspect apply results/oplog when used, and report final facts
   changed, skipped, or still needing human judgment.

## Curation guardrails

- `tracedecay_message_search`, `tracedecay_fact_store_search`,
  `tracedecay_fact_store_get`, and `tracedecay_fact_store_contradict` are
   read-only recall. `tracedecay_fact_store_list`, `tracedecay_fact_store_probe`,
  `tracedecay_fact_store_related`, and `tracedecay_fact_store_reason` provide canonical/derived retrieval, including holographic search
  and scoring semantics. `tracedecay_fact_store_add`, `tracedecay_fact_store_update`, `tracedecay_fact_store_remove`,
  `tracedecay_fact_feedback`, and `tracedecay_dashboard` start/stop mutate
  state or launch a local process; respect host approval/run-mode.
  `tracedecay_memory_status` is a read-only canonical
  fact/entity/trust/feedback/holographic-algebra status snapshot.
- Deletion is permanent: there is no archive, soft-delete, restore, or undo
  path. Prefer update/merge when useful provenance should survive; delete only
  approved stale, duplicate, wrong, secret-like, or user-requested facts.
- Never store secrets, credentials, API keys, or PII. Do not lower trust merely
  because a fact is old; cite the newer evidence or contradiction.
- Dashboard curation can apply hard deletes. Use preview/dry-run first when
  available and surface high-risk delete/merge operations before applying them.
  `POST /api/plugins/holographic/curate` with `dry_run=false` applies
  deterministic duplicate deletion; `POST /api/plugins/holographic/curate/apply`
  applies explicit delete/merge ops.
- Do not let subagents call add/update/remove/feedback tools, apply curation
  ops, or start dashboard mutation flows. Ask them for
  cited evidence, candidate facts, suspected duplicates, and stale/conflicting
  claims, then perform parent-agent validation before writing.
- Hygiene candidates (`secret_like`, `transient`, `supersession`) are review
  evidence, not deterministic apply operations. External LLM plans must use
  strict JSON `{"ops": [...]}` and pass the TraceDecay evidence guard; rejected
  low-confidence or out-of-scope ops must stay skipped.

## Memorize a subject

Use only when the user explicitly asks to memorize or remember a subject, code
area, branch, PR, or decision set.

1. **Research read-only:** TraceDecay graph/search, LCM/session/message tools,
   docs, existing fact searches, and relevant branch/PR context.
2. **Filter:** keep durable, scoped facts with citations. Reject secrets,
   credentials, PII, large code blobs, transient branch state, and uncited
   speculation.
3. **Calibrate trust:** `0.85+` for independently verified decisions, about
   `0.7` for ordinary well-sourced facts, about `0.5` for plausible but
   uncertain facts. Do not ask for approval solely because trust is low.
4. **Dedupe before writing:** search `tracedecay_fact_store_search` with the subject
   plus candidate, matching category, `limit: 10`, `min_trust: 0.5`; skip
   near-duplicates and ask before replacing contradictory facts.
5. **Store accepted facts → `tracedecay_fact_store_add`** with
   content, category, source, tags, entities, trust, and metadata containing
   subject/confidence/citations. Act on `near_duplicate`, `possible_conflict`,
   and `rejected_secret_like`; never rephrase a rejected secret to bypass
   filtering.
