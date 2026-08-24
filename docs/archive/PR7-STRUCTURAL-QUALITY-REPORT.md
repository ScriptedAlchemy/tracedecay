# PR7 structural quality report

> **Archived review record — not implementation authority.** This report
> preserves historical findings and migration evidence. Current requirements
> come only from the `docs/plans/tracedecay-v2/` hierarchy. Exact commit ranges,
> issue matrices, source anchors, test counts, and fix-wave choreography below
> are not rebuild instructions; validate current structure and behavior directly.

Scope: the PR7 memory/facts/provenance arc (`12c32a68..d1057484`, ~52k inserted
lines across 132 files), audited by one line-level review of the contract and
transaction cores plus four delegated deep reviews (application wiring
correctness, schema/migration walk, and two strict structural audits). This
report lists every issue worth addressing — including items deferred from the
in-flight fix wave — filtered to those that align with the V2 plan set's
direction. Each item names its owning plan/PR so nothing here drifts into
unplanned scope.

Verdict context: the four-layer architecture (domain contracts → store crate →
SQL adapter → application) is legitimate and the contract layer is unusually
disciplined (tamper-evident wire types, owner-bound derived identities,
fail-closed constructors, correct transaction envelopes). The debt is
**concentration and duplication**, not design. Migrated-vs-fresh schema
equivalence was verified table-by-table.

---

## P0 — routed into the active fix wave (tracked, do not re-plan)

| # | Issue | Where | Status |
|---|---|---|---|
| P0.1 | Graph consolidation silently drops all `memory_v2_*` data (`merge_one_graph_tx` merges V1 tables only); census test red for 30 tables | `src/migrate/consolidate/sqlite.rs:171` | Fixer F3, with disposition split + merge constraints (union by derived identity, deletion terminality wins, banks/FTS rebuilt not copied) |
| P0.2 | Live fact deletion skips `memory_v2_feedback_history` redaction that canonical purge performs; deleted facts retain API-reachable feedback free-text | `src/store/memory.rs:10834` vs `src/db/memory_v2.rs:4303` | Fixer F1 |
| P0.3 | Dead trio: `fact_retriever`, standalone `purge_memory_v2_fact`, standalone `repair_memory_v2_feedback_history_batch` (wired variants exist; wrappers have zero callers) | `src/db/connection.rs:91,474,614` | Fixer F1 (delete) |
| P0.4 | 45 failing tests from the checkpoint (memory status/repair/eval/curation, MCP handler shapes, storage resolver semantics) | suite-wide | Fixers F1–F3 |

## P1 — pre-push structural fixes (small, high leverage, this PR)

| # | Issue | Fix shape | Plan alignment |
|---|---|---|---|
| P1.1 | Memory tool action names and action strings hardcoded in the generic MCP dispatcher (`inject_trusted_memory_request_id`, `memory_call_requires_operation_context`) | Per-tool `needs_operation_context()` capability queried by the dispatcher; semantics live with the memory handler | Plan 21 (one tool-surface taxonomy); plan 12 dispatcher convergence |
| P1.2 | Action taxonomy triplicated: `action_writes_memory`, the dispatcher gate, the handler dispatch match, implicit untracked-variant list | One action enum/table with `writes()`, `needs_context()`, `has_untracked()` | Plan 21 |
| P1.3 | `server.rs` gratuitous re-nesting of `is_skill_view_tool` inside an `as_object_mut` block the new code doesn't reuse | Flatten back; diff-churn removal | hygiene |
| P1.4 | Two new call sites open-code `MemoryApplication::new(owner, DatabaseFactStore::new(db))` despite the `memory_application_for_db` factory | Route through the factory | Plan 09 application boundary |
| P1.5 | Memory-repair scheduler: fixed 1s retry, decision structure computed then collapsed to a bool, near-duplicate of automation-scheduler plumbing (not a hot-spin — progress is structurally guaranteed — but wrong vocabulary) | Extract to `daemon/memory_repair_scheduler.rs`; drive from a `ReplayPassDecision`-style enum + the shared `replay_backoff` curve (`application/host_admission/replay.rs`) | Plan 32 shared scheduler kernel direction; reverses the `scheduler.rs` 1k crossing |
| P1.6 | `MemoryOperationContext` inline framed-SHA256 duplicates `canonical_framed_sha256` (third duplication of this idiom found this branch) | Reuse the canonical helper | Plan 19 defragmentation |
| P1.7 | No batch-level caps on `FactWriteBatch` events/new_anchors counts (element-level bounds exist) | Add MAX consts mirroring `MAX_FACT_EVIDENCE_REFS` | Plan 02 bounded-everything rule |

## P2 — decomposition wave (mechanical, behavior-preserving, post-green; use `tracedecay_move_symbol`)

| # | Issue | Fix shape |
|---|---|---|
| P2.1 | `src/store/memory.rs` — ~11,870 production lines, 112 `*_tx` fns, 6–8 fused responsibilities | Split to `store/memory/` per audited seams: `harness` (tx envelopes), `scoring`, `search`, `crud`, `curation`, `proposals`, `dashboard`, `projection`, `primitives`, `cutover` |
| P2.2 | Scoring engine duplicated: `compatibility_{jaccard,temporal_decay,term_coverage,tokens,holographic_score}` are formula-identical to `src/memory/retrieval.rs` — silent-drift hazard | One `memory/scoring.rs` parameterized on token source + time unit; delete the copies |
| P2.3 | ~40 isomorphic wrapper methods (`impl FactCompatibilityStore` L423–897) + duplicated read/write envelope pair | Declarative macro or generic dispatcher; one generic `with_write_transaction`/`with_read_snapshot` |
| P2.4 | `MemoryApplication`: ~40 `ensure_owner → authority.X → validate_X` skeletons; 12 near-identical `*_v1`/`*_untracked_v1` shims | Guard-dispatcher keyed on per-op validator; table-driven shims (public API — keep signatures) |
| P2.5 | `src/db/memory_v2.rs` — 6,449 lines incl. 453-line `create_schema` and ~1,220 inline test lines | Split to `db/memory_v2/`: `schema`, `migrations`, `types`, `backfill`, `writers`, `cutover`, `helpers`; tests to sibling file |
| P2.6 | `crates/tracedecay-store/src/memory.rs` — 1,247 symbols of DTO boilerplate, no module boundaries | Split: `queries`, `receipts`, DTO groups by domain (`search`/`curation`/`proposal`/`dashboard`), `traits`; tests out. Stretch: getter-derive macro |
| P2.7 | `src/application/memory.rs` — 4,020 lines incl. ~1,259 test lines | Split to `application/memory/`: `error`+`context`, `sanitize`, `anchors`, `compatibility`, `dashboard`, `v1_api`; tests out |
| P2.8 | `src/dashboard/memory_service.rs` — genuine 1k production crosser | Split to `memory_service/`: `facts`, `graph`, `projection`, `similarity`, `curation`, `oplog` |
| P2.9 | `handle_fact_store_for_target` 252-line god-match: five read arms hand-roll identical tracked/untracked dispatch | One helper over the P1.2 action table (~−100 lines, removes `cross_project_selector` threading) |
| P2.10 | `memory_service` curation ops return `(Value, bool)` where the bool restates the embedded status; 3–4 hand-built error envelopes each | `curation_error(op, id, msg)` helper; derive ok from status |
| P2.11 | Deep-complexity tx bodies: `related_compatibility_facts_tx` (230 lines, fan-out 62), `compatibility_rewire_merge_relations_tx` (cyclomatic 20, nesting 4), `promote_..._with_disposition_tx` (223 lines) | Internal extraction during P2.1; keep transaction boundaries identical |
| P2.12 | `FactWriteBatch::into_parts` 9-tuple (`clippy::type_complexity` silenced) | Parts struct (precedent: `RetrievalAnchorRecordV2Parts`) |
| P2.13 | `db_error` catch-all mapper with fan-in ~195 | During P2.5: audit call sites for failure modes that deserve distinct variants; keep the funnel where uniform |
| P2.14 | `purge_*`/`quarantine_fact` adjacent free fns with overlapping `_inner` splits | Cohere into a `purge` module in P2.5; verify shared invariants once |
| P2.15 | MCP handler arg-coercion helpers (~15 tiny fns) | Optional `handlers/memory/args.rs` grouping during P1.1 work |

## P3 — planned-PR alignment (address in the named slice, not now)

| # | Issue | Owning plan / PR |
|---|---|---|
| P3.1 | Authorization snapshots are namespace-derived placeholders (`build_resolution_authorization_v1` hashes the authority namespace; not policy-engine-issued grants) | Plan 06 / PR11 — replace with real typed grants; resolution-side recheck contract already correct |
| P3.2 | Plan-05 PR7 query gaps: redacted frontiers, unknown/hidden denominators, positive contradiction detection — response types don't yet expose the counts | Plan 05 — either late-PR7 follow-up or explicit PR8 hand-off; requires typed response fields plus a public query path |
| P3.3 | Dirty-state / index-tree positive classification always `Unknown` (no status probe in PR7 by design; "when available") | Plan 36 / PR9 read-only Git intelligence |
| P3.4 | Provider-literal special cases in `observation_projection/apply.rs` (claude legacy dual-path derive, `owns_lcm_raw`-style checks, codex goal-dedupe, cursor namespace) + lazy `upgrade_v1_claude_source_path` | Plan 19 / PR19 defragmentation + migration cutover — convert to provider capability methods and a one-time projector-version transition |
| P3.5 | `sync_directory` durability primitive hosted in `application/host_admission` but consumed by `db`/`tracedecay`/`branch` (downward reference; works, wrong home) | Plan 19 — move to a low-level fs/durability module during defragmentation |
| P3.6 | Daemon cannot self-detect runtime FTS desync (only doctor/restart finds it); interrupted bulk load leaves no sentinel | Plan 14 (health/recovery kernel) — scheduler-driven periodic quick-check + in-place FTS rebuild under the writer lane |
| P3.7 | Offline consolidation vs daemon auto-respawn contention (offline windows get refilled by host tool calls; operator races the respawn) | Plan 14 / plan 19 — a typed maintenance-window API that hosts respect, replacing ad-hoc stop/kill |
| P3.8 | Largely unconsumed research-contract surface in `tracedecay-domain` (manifest/catalog/tombstone types beyond current consumers) | Plan 13 — consumers land PR8+/PR13; keep, do not delete; re-audit at PR13 |
| P3.9 | Redundant v20 named unique indexes (`idx_memory_v2_proposals_owner_*`) vs fresh-path autoindexes — benign divergence | Plan 19 / PR19 — drop during cutover consolidation; harmless until then |
| P3.10 | Backfill/cutover crash-point replay not exhaustively exercised (structural gating verified; per-stage crash matrix not) | Direct PR19 migration behavioral tests — extend crash-point coverage over `backfill_memory_v2_batch`/`finalize_memory_v2_cutover` stages |
| P3.11 | Anchor coverage thin spots: store-path copied-prompt binding; end-to-end project-move re-resolution | Plan 13 direct behavioral tests — small additions once PR7 cargo gates are green |
| P3.12 | `load_digest_targets` silently degrades a corrupt digest manifest to empty via `unwrap_or_default` | Plan 14 — surface as a typed warning through doctor/observability |

## Explicitly not adopted (reviewed and rejected)

- "Hot-spin risk" in the memory-repair scheduler — disproved: `Advanced`
  requires a non-empty batch and errors stop the loop. P1.5 stands on
  structural grounds only.
- Deleting `memory_application_for_db` as a thin wrapper — it is a legitimate
  owner+db→application factory; the fix is to *use* it (P1.4).
- Treating `staged_notice`'s signature migration as churn — the
  `dashboard_root` parameter is genuinely still used; no action.
- Collapsing the four-layer memory architecture — the layering is real
  (contracts/adapter/service/schema), not duplicated concepts; only the
  scoring engine (P2.2) is a true parallel implementation.
