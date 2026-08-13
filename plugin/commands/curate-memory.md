---
description: Run or inspect final-V2 agent-managed memory curation and its terminal run record.
argument-hint: "[subject]"
---

# Curate memory

Interpret `$ARGUMENTS` as the fact, entity, query, or curation scope. If absent,
resolve the active project and run the canonical agent-managed curator.

1. Resolve scope: confirm the active project root/store with `tracedecay_active_project` before touching memory.
2. Start read-only with `tracedecay_fact_store_search`, `tracedecay_fact_store_list`, `tracedecay_fact_store_get`, `tracedecay_fact_store_probe`, `tracedecay_fact_store_related`, `tracedecay_fact_store_reason`, or `tracedecay_fact_store_contradict`. Use `tracedecay_memory_status` only when the user asks for its read-only canonical fact/entity/trust/feedback/holographic-algebra status snapshot. Open `tracedecay_dashboard` (`action: "start"`) only when the user wants visual curation.
3. Run the sole public semantic launcher, `fact_store_curate`, through the MCP
   adapter `tracedecay_fact_store_curate` or generic CLI adapter `tracedecay
   tool fact_store_curate`, with only `fact_review_limit` and
   `min_confidence_millionths`. Capture the returned run id. The request
   accepts no caller-selected task, operations, run identity, or effect
   authority; validation and supported canonical mutations finish inside the
   daemon-owned run.
4. Inspect it without mutation: call `tracedecay_automation_run_list`
   (`limit?`), then `tracedecay_automation_run_view` (`run_id`). CLI
   equivalents are `tracedecay automation runs list --json` and `tracedecay
   automation runs view <run_id> --json`. Report terminal status, validation
   report, and applied/rejected operations.
5. If the record advertises an artifact kind, read it with
   `tracedecay_automation_run_artifact_view` (`run_id`, `kind`) or `tracedecay
   automation runs artifact <run_id> <kind> --json`. Do not invent an artifact
   kind.
6. The HTTP adapter is `POST /api/application/retained/fact_store_curate`; HTTP
   inspection equivalents are `GET
   /api/automation/runs`, `GET /api/automation/runs/{run_id}/artifacts`, and
   `GET /api/automation/runs/{run_id}/artifacts/{kind}`.
7. Use direct `tracedecay_fact_store_add`, `tracedecay_fact_store_update`,
   `tracedecay_fact_store_remove`, or `tracedecay_fact_feedback` only for an
   exact administrative instruction; these are independent retained
   operations. Deletion is permanent. If the requested deletion target is
   ambiguous, show the resolved fact id and content summary and confirm only
   that target before removal.
8. Verify read-only with canonical fact queries and memory status. If a failed
   run already records applied operations, report the committed effects and
   required reconciliation instead of rerunning it blindly.

Output: run id, terminal status, facts changed/skipped, applied/rejected
operations, artifacts inspected, any reconciliation required, and the final
verification result.
