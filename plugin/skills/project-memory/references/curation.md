# Memory curation

Full curation and memorize-a-subject protocol for `tracedecay:project-memory`.

- Curate: read-only inventory, automatic run, terminal ledger, verify
- Curation guardrails (deletion, subagents, dashboard)
- Memorize a subject on explicit request

## Curate (agent-managed mutation)

Use subagents only for scoped inspection or recommendation work, with explicit
project selectors and non-overlapping ownership. Final-V2 curation is one
agent-managed run: evidence collection, validation, automatic canonical
application, and a terminal run record. The user may launch it and observe its
result, but there is no second per-operation action.

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
3. **Run the canonical curator:** use `tracedecay_fact_store_curate`
   (`fact_review_limit`, `min_confidence_millionths`), `tracedecay tool
   fact_store_curate`, or `POST /api/application/retained/fact_store_curate`. The runner
   reads bounded canonical facts, validates backend output, automatically
   applies supported policy-valid operations, and returns a run id. The MCP
   trigger accepts only its two optional review bounds; run identity, task,
   operations, validation, and effect authority remain daemon-owned.
4. **Inspect the run read-only:** use `tracedecay_automation_run_list`
   (`limit?`) and then `tracedecay_automation_run_view` (`run_id`), or their CLI
   equivalents `tracedecay automation runs list --json` and `tracedecay
   automation runs view <run_id> --json`. Read `status`, `validation_report`,
   `applied_ops`, `rejected_ops`, and `artifacts` from the terminal record. The
   list projection exposes descriptors as `artifact_kinds`. For each advertised
   artifact needed for the review, use
   `tracedecay_automation_run_artifact_view` (`run_id`, `kind`) or `tracedecay
   automation runs artifact <run_id> <kind> --json`.
5. **Inspect through HTTP when appropriate:** `GET /api/automation/runs` lists
   runs, `GET /api/automation/runs/{run_id}/artifacts` verifies the published
   artifact chain, and `GET
   /api/automation/runs/{run_id}/artifacts/{kind}` returns one verified
   payload. These are observation surfaces.
6. **Inventory the evidence:** group relevant facts into add, update,
   merge/dedupe, stale, contradiction, secret-like, transient, supersession,
   and possible hard-delete buckets. Keep fact ids, source/provenance, trust,
   tags, entities, evidence links, and counterevidence with each candidate.
7. **Research gaps:** use TraceDecay graph/search plus LCM/session/message tools
   to mine past sessions, raw messages, summary DAGs, branch/PR context, docs,
   and tests. Scoped subagents may research bounded read-only questions only;
   the parent agent is the sole memory writer and must review raw findings
   before trusting them.
8. **Interpret terminal state:** report succeeded, failed, or skipped status
   and the recorded applied and rejected operations. If a failed record already
   contains applied operations, report the committed effects and required
   reconciliation; never rerun blindly.
9. **Use direct fact commands only for exact administration:**
   `tracedecay_fact_store_add`, `tracedecay_fact_store_update`, and
   `tracedecay_fact_store_remove` are independent retained operations, not a
   continuation of a curator run. Removal requires an exact user instruction
   because it is permanent. If the user's requested target is ambiguous, show
   the resolved fact id and content summary and confirm that exact target
   before removal.
10. **Verify read-only:** re-run search/list/probe/related/contradict/get as
   appropriate, inspect direct-command results and oplog when used, and report
   final facts changed, skipped, or still needing human judgment.

## Curation guardrails

- `tracedecay_message_search`, `tracedecay_fact_store_search`,
  `tracedecay_fact_store_get`, and `tracedecay_fact_store_contradict` are
   read-only recall. `tracedecay_fact_store_list`, `tracedecay_fact_store_probe`,
  `tracedecay_fact_store_related`, and `tracedecay_fact_store_reason` provide canonical/derived retrieval, including holographic search
  and scoring semantics. `tracedecay_fact_store_add`, `tracedecay_fact_store_update`, `tracedecay_fact_store_remove`,
  `tracedecay_fact_feedback`, and `tracedecay_dashboard` start/stop mutate
  state or launch a local process; respect host execution policy.
  `tracedecay_memory_status` is a read-only canonical
  fact/entity/trust/feedback/holographic-algebra status snapshot.
- Deletion is permanent: there is no archive, soft-delete, restore, or undo
  path. Prefer update when useful provenance should survive. An exact user
  instruction naming the fact and deletion intent is sufficient; ask a scoped
  safety question only when the target or intent is ambiguous.
- Never store secrets, credentials, API keys, or PII. Do not lower trust merely
  because a fact is old; cite the newer evidence or contradiction.
- Dashboard automation launches and observes the same agent-managed curator;
  it is not a separate curation authority.
- Do not let subagents call add/update/remove/feedback tools or run curation
  operations. Ask them for
  cited evidence, candidate facts, suspected duplicates, and stale/conflicting
  claims, then perform parent-agent validation before writing.
- Backend output must use strict JSON `{"ops": [...]}` and pass the TraceDecay
  evidence and policy guards; rejected low-confidence or out-of-scope ops stay
  terminally rejected or quarantined.

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
   uncertain facts. Low trust alone does not require a user decision.
4. **Dedupe before writing:** search `tracedecay_fact_store_search` with the subject
   plus candidate, matching category, `limit: 10`, `min_trust: 0.5`; skip
   near-duplicates and ask before replacing contradictory facts.
5. **Store accepted facts → `tracedecay_fact_store_add`** with
   content, category, source, tags, entities, trust, and metadata containing
   subject/confidence/citations. Act on `near_duplicate`, `possible_conflict`,
   and `rejected_secret_like`; never rephrase a rejected secret to bypass
   filtering.
