# Gap ledger — PR8 through PR14

**Purpose.** A requirement-by-requirement reconciliation of what the V2 plan set
specifies against what the code at integration `330e47a0e` actually does. This
is not a staleness sweep: the rejection register, attribution corrections, and
markdown repairs landed separately in `c32d9701d`, `256d64612`, and `9dcdd0d90`
and are preserved unchanged. This file answers a different question — **for each
PR8–PR14 requirement, is it implemented, and is it reachable?**

**Scope.** PR8 through PR14. PR14 is exactly the twelve existing workspaces
(Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery,
Automations, Observatory, Costs, Settings); Work belongs to PR17 under Plan 24
and is out of scope. Plan 11b (structure visualization) is in PR14 scope by the
2026-07-26 owner decision. Anything scoped PR15 or later is noted and skipped.

**Companion, not replacement.** [`NEXT.md`](NEXT.md) remains the active
PR12/PR13 execution slice. This file is the audit ledger; where the two
disagree, the evidence cited here is authoritative and `NEXT.md` should be
corrected.

## Evidence rules used here

1. A plan sentence claiming something shipped is **not** evidence. Only source
   code is. Every `IMPLEMENTED` row cites `file:line`.
2. "The code exists" is the weakest possible evidence. A requirement counts as
   implemented only when it is **reached**: a non-test production caller, a
   registered route, a declared test module, a gate that can fail.
3. `cargo test-all` has never produced a passing run in this checkout. Nothing
   backend is marked `IMPLEMENTED AND VERIFIED` on the strength of tests
   existing. `IMPLEMENTED BUT UNVERIFIED` is the correct status for almost all
   backend work today, and that is not a defect — it is an honest status.

Status vocabulary: `NOT IMPLEMENTED`, `PARTIALLY IMPLEMENTED`,
`IMPLEMENTED BUT UNREACHABLE`, `IMPLEMENTED BUT UNVERIFIED`,
`IMPLEMENTED AND VERIFIED`, `SUPERSEDED`.

**Provenance.** Sections P0, P1, and P2 were verified directly against the code
by the auditor. Section P4 aggregates six parallel plan-by-plan audits; its rows
carry their cited evidence but were not each re-derived, and it says so. Where a
parallel audit's claim was load-bearing it was re-checked, and two of them were
refuted — see P1-d and P1-e. A lane's own report is not evidence.

---

## P0 — Reachability breaks that make a shipped capability dead

These are the signature defect of this codebase: complete, correct, sometimes
tested implementations that no production path can reach. They are listed first
because each one is cheap to close and currently makes a PR14 user journey
impossible.

### P0-1 · Doctor remediation is unreachable from the dashboard (contract break)

**Plan requirement.** PR14's stated user outcome is that a user can "execute a
legal remediation" from the dashboard
([`00-plan-set-index.md:418-420`](00-plan-set-index.md)); Plan 09 owns the one
Doctor use case and its legal-remediation handoffs
([`00-plan-set-index.md:307-309`](00-plan-set-index.md)).

**Status.** `NOT IMPLEMENTED` (end to end). Backend complete and production-
wired; the frontend cannot construct a valid request.

**Evidence.**

- Backend requires `target` on both requests, with `deny_unknown_fields`:
  `src/dashboard/doctor_remediation_api.rs:589-604`.
- Routes registered: `src/dashboard/mod.rs:1095-1103`.
- Dispatcher is genuinely production-wired (the old "no production caller" bug
  is fixed): `src/daemon.rs:3239`, `src/daemon.rs:3291`, `src/daemon.rs:4304`,
  `src/daemon.rs:4355`.
- Generated contract makes `target` required:
  `dashboard/src/contracts/generated.ts:509-512` (preview) and
  `dashboard/src/contracts/generated.ts:451-457` (apply).
- Frontend omits `target` in both calls:
  `dashboard/src/data/query/doctor.ts:27` parses `{ operation }` only, and
  `dashboard/src/workspaces/observatory/DoctorInspector.tsx:156-162` mutates
  with `{operation, preview_id, idempotency_key, confirmed}`.

Because both call sites run `Schema.parse(...)` before `fetch`, preview and
apply throw a `ZodError` **before any HTTP request is issued**. The failure is
not a rejected request; the button does nothing.

**A second, independent break in the same surface.** The Doctor UI also consumes
an `authority_scope` concept that exists nowhere in the wire contract or the
backend:

- `dashboard/src/contracts/` contains exactly three files (`generated.ts`,
  `index.ts`, `wire.ts`) and **no occurrence of `AuthorityScope`** in any of
  them.
- The Rust `DoctorRemediationOperationV1`
  (`src/dashboard/doctor_remediation_api.rs:144-155`) has ten fields and no
  `authority_scope`.
- The frontend nonetheless imports the type and its schema from the contracts
  barrel: `dashboard/src/workspaces/observatory/doctorModel.ts:4` and `:12`,
  `dashboard/src/workspaces/observatory/DoctorInspector.tsx:7`, and builds a
  local schema around it at `doctorModel.ts:23`.

These imports resolve to nothing, so they are hard TypeScript errors in addition
to the missing-`target` errors. The Observatory Doctor module does not
type-check at integration HEAD for two unrelated reasons, and the fix for one
does not fix the other. Either the backend must serve `authority_scope` on the
operation or the frontend must stop modelling it; that is a product decision,
not a mechanical repair.

**When it broke.** `target` was added to the generated preview schema by
`203e4c266 fix(dashboard): verify all contract drift layers`, tonight's
contracts regeneration, without updating either caller.

**Collateral.** This is a compile-time type error against
`DoctorRemediationApplyRequest`, so `npm run typecheck`
(`.github/workflows/ci.yml:858`) fails, which means the CI `dashboard` job is
red at integration HEAD. That job is a real gate — `npm run typecheck`,
`npm run contracts:check`, and `npm test` are pinned into it by
`tests/dashboard_workflow_contract_test.sh:61-66`, which itself runs in CI at
`.github/workflows/ci.yml:112-113`.

**What remains.** Thread a `DoctorRemediationTargetV1` from the selected
descriptor through `remediationForEntry` into both call sites. The eleven target
variants are enumerated at
`src/dashboard/doctor_remediation_api.rs:61-75`.

### P0-2 · Plan 11b Surfaces 1–2: backend shipped, no consumer exists

**Plan requirement.** Plan 11b Surface 2 (Transit) names two endpoints
explicitly and instructs "register the existing `get_call_chain`"
([`11b-structure-visualization.md:77-79`](11b-structure-visualization.md));
Surface 1 (Anatomy) requires per-symbol drill-in.

**Status.** `IMPLEMENTED BUT UNREACHABLE`. Every backend half exists,
is routed, and is covered by a declared test; nothing in the dashboard fetches
any of it.

**Evidence.**

- Handlers: `src/dashboard/graph_structure_api.rs:245` (`call_chain`), `:342`
  (`strata`), `:482` (`node_facts`), `:606` (`node_tests`), `:732`
  (`node_sessions`).
- Routes registered: `src/dashboard/mod.rs:1013-1030`.
- Wire contracts generated: `dashboard/src/contracts/generated.ts:1135-1163`
  (`StructureReadV1`, `StructureReadV12`), `:83-94`
  (`CallChainMeasurementV1`), `:1110-1122` (`StrataMeasurementV1`).
- Backend tests exist and their module **is** declared:
  `tests/dashboard_api_test/graph.rs:631-700`,
  `tests/dashboard_api_test/main.rs:23`.
- **No frontend consumer.** No file under `dashboard/src` references
  `/api/plugins/graph/call-chain`, `/api/plugins/graph/strata`, or the
  `node/{id}/facts|tests|sessions` paths, and the generated `StructureRead`
  schemas are imported nowhere outside `generated.ts`.

**Note.** Plan 11b:77's instruction to register `get_call_chain` is **done** —
`src/dashboard/mod.rs:1013`. That plan line is stale and should say so. The
remaining gap moved one layer up: the route exists, the surface does not.

**What remains.** Build the Anatomy and Transit surfaces against the five
endpoints, or formally defer Plan 11b Surfaces 1–2 and say so in the plan.
Shipping registered-but-unconsumed routes is the worst of both states.

### P0-3 · Automations is a read-only viewer; its whole action surface is dead

**Plan requirement.** PR14 requires an *operable* product — Automations is one
of the twelve workspaces and the plan set treats automation runs, skill
approval, and scheduler control as product behavior
([`00-plan-set-index.md:429-431`](00-plan-set-index.md)).

**Status.** `IMPLEMENTED BUT UNREACHABLE`.

**Evidence.**

- The page issues exactly four reads and no writes:
  `dashboard/src/workspaces/automations/AutomationsPage.tsx:110`
  (`/api/automation/scheduler/status`), `:113` (`/api/automation/jobs`), `:116`
  (`/api/automation/skills`), `:121` (`/api/automation/fact-proposals`).
- There is **no** `useMutation` and no `method: 'POST'` anywhere under
  `dashboard/src/workspaces/automations/` outside tests.
- Registered but never called by the dashboard: `/api/automation/scheduler/pause`,
  `/api/automation/scheduler/resume`, `/api/automation/run/memory-curator`,
  `/api/automation/run/session-reflection`, `/api/automation/run/skill-writing`,
  `/api/automation/runs/{run_id}/artifacts`,
  `/api/automation/runs/{run_id}/artifacts/{kind}`,
  `/api/automation/skills/draft`, the `skills/{id}/approve|archive|disable|
  discard-update|restore` actions, the `fact-proposals/{id}/apply|reject`
  actions, and `/api/automation/outcomes` — all in
  `src/dashboard/mod.rs`.

**What remains.** Either wire the actions (they are the reason the routes
exist) or record Automations as a deliberately read-only PR14 surface and move
the action wiring to a named later slice. It must not stay ambiguous.

### P0-4 · `tests/dashboard_api_test/loom.rs` is committed but never compiled

**Status.** `NOT IMPLEMENTED` as verification. The file is real and
substantive, and it is the **only** backend test of `GET /api/loom/temporal`.

**Evidence.** `tests/dashboard_api_test/main.rs:13-29` declares every sibling
module except `loom`. The test at `tests/dashboard_api_test/loom.rs:6-96`
asserts recorded session ends, edited-file projection, branch/worktree spans,
and the four `source_statuses` states including the `delivery_outcomes`
unsupported-authority string.

**Consequence for plan status.** "Loom temporal projections" is currently
treated as a closed item. It is closed at the implementation layer only; its
sole proving test cannot run, so it must remain `IMPLEMENTED BUT UNVERIFIED`
even after `dashboard_api_test` first passes.

**Bounding the pattern.** A scan of all fifteen multi-file suites under `tests/`
found exactly one undeclared module: this one. The declaration defect is not
widespread.

**What remains.** Add `mod loom;` to `tests/dashboard_api_test/main.rs`, then
fix whatever it reports.

### P0-5 · `branch remove` receipt refusal has no regression test

**Status.** `IMPLEMENTED BUT UNVERIFIED`, with zero coverage.

**Evidence.** The guard exists and is production-reached:
`src/migrate/memory_cutover.rs:228` (`verify_branch_removal_receipts`), called
at `src/daemon/branch_admin.rs:920` and `:995`. **No test anywhere references
it.** The only branch-removal tests are happy-path deletions:
`tests/core_cli_suite/cli_non_interactive_test.rs:1666` and `:1694`.

**Why it matters.** This is a destructive-operation refusal path. Per the
retained safety baseline, destructive operations keep CAS, confirmation, and
rollback; a refusal with no regression test is exactly the kind of guard that
silently stops refusing.

**What remains.** One regression proving `branch remove` refuses when required
removal receipts are absent or mismatched.

### P0-6 · The feedback Doctor family can never advertise a legal action

**Status.** `IMPLEMENTED BUT UNREACHABLE`.

**Evidence.** The Doctor kernel decides whether a surface is mounted before it
will return any legal remediation action. Every other surface computes a real
condition; the feedback surface is hard-coded false:

```1292:1292:src/daemon/doctor_kernel.rs
                    tracedecay_application::doctor::DoctorOwningSurfaceV1::FeedbackRead => false,
```

and three lines later `if !mounted { return Vec::new(); }`. Any remediation
registered against `FeedbackRead` is therefore permanently invisible, regardless
of descriptor state.

**What remains.** Decide whether this is a deliberate not-yet-mounted marker or
an oversight. If deliberate it belongs in the plan as a named deferral with the
condition that will flip it; a bare `false` in a match arm is not a status
anyone can audit.

### P0-7 · Two retention engines run on defaults that disable them

**Status.** `IMPLEMENTED BUT UNREACHABLE` in effect — scheduled, called, and
configured to do nothing.

**Evidence.**

- Observation evidence retention is fully built and the daemon calls it
  (`src/daemon/git_watch/store_maintenance.rs:216-224`), but
  `ObservationRetentionConfig::default()` sets `enabled: false` with all three
  release windows `None` (`src/global_db/observation/retention.rs:174-182`). It
  is a scheduled no-op.
- LCM retention defaults `enabled: true` but `offload_after_days: None` and
  `drop_after_days: None`, leaving only `dedupe_projected_after_days: Some(30)`
  (`src/sessions/lcm/retention.rs:128-139`). Deduplication runs; offload and
  drop never do.

Combined with P1-a below, all three retention mechanisms that Plan 38 §3–§4
relies on are inert or near-inert by default. Plan 38's storage-size problem is
not addressed by any current default.

---

## P1 — Corrections: items believed open that the code shows are closed

Recording these matters as much as recording gaps. Several carried "open" items
are wrong at HEAD, and repeating them wastes lanes.

| Item | Prior belief | Actual state at `330e47a0e` | Evidence |
|---|---|---|---|
| Plan 38 session retention | "apparently never implemented" | Implemented and daemon-scheduled, but **inert by default** by deliberate design | `src/retention.rs:263`, `:297`; scheduled `src/daemon/scheduler.rs:1221`; Doctor path `src/daemon/doctor_kernel.rs:1453`, `:1470` |
| Sigma "Container has no width" | three page errors on graph surfaces | Single Sigma instantiation site, guarded by a zero-dimension check with a `requestAnimationFrame` retry | guard `dashboard/src/viz/graph/GraphCanvas.tsx:243-251`; only construction `:578` |
| `get_call_chain` unregistered | route missing | Registered | `src/dashboard/mod.rs:1013`; handler `src/dashboard/graph_structure_api.rs:245` |
| Doctor dispatcher had no production caller | 3 routes returned `unsupported` | Production dispatcher constructed and attached | `src/daemon.rs:3239`, `:3291`, `:4304`, `:4355` |
| `StorageFindingsPayloadSchema` describes `{kinds}` vs served `kind_statuses`, hand-maintained against no Rust type | mismatch | Agrees, and **is** backed by a Rust type | alias `dashboard/src/contracts/generated.ts:1523`; field `:414`; Rust `src/dashboard/doctor_findings_api.rs:49` |
| Memory cutover never invoked | unreachable | Production CLI caller exists | `src/commands/migrate.rs:157-165` |

### P1-a · Plan 38 retention: the precise, non-obvious truth

The mechanism is real and runs, but session retention prunes nothing out of the
box. `RetentionConfig` defaults `session_messages_days: None` and
`lcm_raw_messages_days: None` (`src/retention.rs:74-82`), documented at
`:58-65` as intentional because those rows are "part of the lossless session
record". Only `analytics_events` prunes by default, at 180 days
(`src/retention.rs:48`).

So the accurate statement is: **Plan 38 §3 is implemented and scheduled, and
contributes zero bytes by default.** The 256 GB → 75 GB driver Plan 38 cites is
therefore not addressed by session retention defaults, and any plan text
implying it is should be corrected. Whether the lossless default is right is an
owner question, not an implementation gap.

### P1-b · Production purge: exists, but only behind `migrate`

The standalone wrapper `purge_memory_v2_fact` is `#[cfg(test)]`-gated
(`src/db/memory_v2/writers/purge.rs:34`) and called only from
`src/db/memory_v2/tests.rs`. The real work function
`purge_memory_v2_fact_inner` (`:136`) is **not** test-gated and has one
production chain: `src/db/memory_v2/backfill/oplog.rs:74` ←
`src/db/memory_v2/cutover.rs:132` ← `src/commands/migrate.rs:165`.

A production purge path therefore exists, but it is reachable only through the
migrate command's cutover, not as an ordinary retention or deletion operation.
The docstring at `purge.rs:31-33` describing "the production purge path" is
accurate but easy to misread as a general-purpose path.

### P1-c · Project identity keys on repository, not path — with one hole

Identity resolution at HEAD does key on repository identity: the registry looks
up `code_projects` by `git_common_dir` (`src/global_db/project_registry.rs:36`)
and records repo-identity aliases on upsert (`:717`). The resolution ladder is
`src/global_db/project_registry.rs:1243-1272`: in-repo identity marker → project-
root path alias → `git_common_dir` alias. Tier 3 is what collapses worktrees of
one repository into one project, and it is present.

The hole is tier 1: `write_repository_identity_marker`
(`src/storage.rs:547`) has **no ordinary production caller**. Its only callers
are `src/search_eval/candidate_output.rs:3289`, two benchmark runners
(`src/sessions/claude_observation_benchmark/runner.rs:361`,
`src/sessions/session_temporal_benchmark.rs:693`), and doctor/heal plus unit
tests. Ordinary project open never writes a marker, so identity always depends
on alias rows surviving.

**Correction to the above, on re-verification.** The ladder is real, but it is a
*lookup*, and what happens when the lookup misses matters more than the ladder
does. The default mint is purely path-derived:

```587:595:src/storage.rs
pub fn default_profile_project_id(project_root: &Path) -> String {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("proj_{}", &digest[..16])
}
```

and the store directory is `profile_root/projects/{project_id}`
(`src/storage.rs:583-585`). So when no marker exists (tier 1 never fires — see
above) and no alias row has been recorded yet, a new checkout path mints a
**new store shard** keyed on its own canonicalized path. Two worktrees of one
repository collapse into one project only if an alias row already links them.
That is the mechanism by which store count tracks checkout paths rather than
repositories.

The observed 444 store directories at ~101 GB against ~37 real repositories is
runtime data this audit could not inspect under the freeze, so the *number* is
not certified here. The *mechanism* that would produce it is confirmed in code
above. Recommended framing: a reconciliation task against the existing Doctor
relink/retirement machinery
(`src/global_db/registered_dashboard.rs:222-360`), plus a decision about whether
ordinary project open should write an identity marker.

### P1-d · Refuted: "the integration test suites are undeclared"

Two of the six parallel audits reported that `dashboard_api_test` and roughly
twenty other multi-file suites have no `[[test]]` entry in `Cargo.toml` and
therefore cannot run — one concluded the CI invocation would fail outright.
**This is false.** `autotests` is not disabled, so Cargo auto-discovers
`tests/<name>/main.rs` as a test target named `<name>`. `cargo metadata` on this
worktree reports **44 test targets**, including `dashboard_api_test`,
`hooks_lsp_suite`, `mcp_suite`, `daemon_suite`, `graph_suite`, `session_suite`,
`code_index_suite`, and `storage_runtime_suite`.

The declaration defect is exactly what P0-4 says it is and no larger: one
undeclared `mod loom;` inside an otherwise healthy suite. Recorded here because
the false version of this claim is alarming, plausible, and would have sent a
lane on a large pointless refactor.

### P1-e · Refuted: "MCP `tracedecay_call_chain` is unregistered"

One audit reported the MCP tool as built-but-unreachable, citing an
`#[allow(dead_code)]` handler and its absence from the graph dispatch match.
That handler is a dead duplicate; the live tool is registered through the
application surface: `def_call_chain_read()` is listed in the catalog at
`src/mcp/tools/definitions.rs:397` and
`src/mcp/tools/definitions/application.rs:999`, and dispatches via
`ApplicationSurfaceOperation::CallChain`
(`src/application_surface.rs:112`, `:226`, `:1899-1901`). The stale duplicate is
`def_call_chain()` at `src/mcp/tools/definitions/graph.rs:691`.

The real finding here is smaller and different: **a dead duplicate definition
should be deleted**, because its `dead_code` annotation and its "not yet
registered" comment actively mislead audits — as it just did.

---

## P2 — Plan text that is now wrong

| Plan | Line | Says | Should say |
|---|---|---|---|
| `11b-structure-visualization.md` | 77-79 | "register the existing `get_call_chain`" as pending work | Registered at `src/dashboard/mod.rs:1013`; the open work is the missing frontend surface (P0-2) |
| `00-plan-set-index.md` | 470-472 | Loom time boundaries listed among implemented checkpoint items | Accurate, but must note its only backend test is undeclared (P0-4) and cannot contribute to verification |
| `00-plan-set-index.md` | 474-478 | Checkpoint is "implemented but unverified" pending `dashboard_api_test` | Correct and should be kept. Add that the suite currently cannot pass because the CI `dashboard` job is red on `npm run typecheck` (P0-1) |
| `NEXT.md` | 140-159 | Plan 38 fully delivered including "§3 session retention" | Delivered as mechanism; inert by default. State the default explicitly (P1-a) |
| `38-storage-retention-size-and-efficiency.md` | 11-16 | "All seven product-contract sections are implemented" | §3 and §4 are dedupe-only by default and observation retention is disabled (P0-7, P1-a) |
| `13-research-provenance-and-context-anchors.md` | 19-23 | "Landed / implemented across …" unqualified | GitHub-stack anchor targets remain pending, as the same plan admits at `:27-28` |
| `26-observability-accounting-and-usage.md` | 5-6 | "Cross-cutting instrumentation is implemented" | Observatory and Costs surfaces are unverified; the dashboard suite has never gone green |
| `14-historical-failure-regression-matrix.md` | 7-10 | Read by several readers as a numbered failure ledger | The plan explicitly disclaims that; it defines regression *classes* per PR slice |

One **source comment** is also wrong and worth fixing when someone is next in
the file, though this ledger does not own source:
`src/global_db/observation/retention.rs:76-81` says the module is "reachable
only from retention tests" until the daemon seam is wired. The daemon has since
wired it at `src/daemon/git_watch/store_maintenance.rs:216-224`; the comment and
its `dead_code` allowance are stale, and they conceal the real reason nothing
happens, which is the `enabled: false` default (P0-7).

**Deliberately unchanged.** The rejection register
(`00-plan-set-index.md:115-275`), the retained-ownership rules (`:277-320`), the
PR14/PR17 Work allocation as plan authority rather than user rejection
(`:146-155`), product-runtime receipts as required, and worktrees/Kanban as real
product features all remain as the earlier passes left them. Nothing in this
audit contradicts them.

---

## P3 — Candidates to drop rather than carry forward

1. **Plan 11b Surface 3 (Disagreement field).** Already premise-gated by three
   unresolved gates (`11b-structure-visualization.md:91-99`), one of which
   requires a session→file materialization that does not exist. Surfaces 1 and 2
   are not built yet either (P0-2). Carrying a third, harder surface in PR14
   scope is not credible. Recommend explicit deferral with the gates preserved.
2. **The duplicated hand-written storage-findings shape.**
   `dashboard/src/workspaces/observatory/contracts.ts:30` re-declares
   `kind_statuses` with a hardcoded `.length(5)` alongside the generated
   `StorageFindingsPayloadSchema`. The workspace rule is that the generated
   contracts module is the only Rust-to-dashboard wire boundary and hand-written
   shapes are forbidden. Recommend deleting the duplicate rather than
   maintaining two sources of truth.
3. **Plan 34's API-migration planner.** Nothing in `src/` implements it, no
   other plan depends on it, and the rename-preview path already covers the
   refactoring need that PR8–PR14 actually exercises. Recommend dropping it from
   the pre-PR14 slice explicitly rather than letting it sit unimplemented and
   unmentioned.
4. **Plan 13's GitHub-stack anchor targets.** The plan already admits these are
   pending (`13-research-provenance-and-context-anchors.md:27-28`) while its
   header reads as delivered. Since PR13's GitHub work is read-only ingest and
   never posts, anchoring into the GitHub stack is not on any PR8–PR14 journey.
   Recommend a named deferral so the header stops overclaiming.
5. **The dead `def_call_chain()` duplicate** at
   `src/mcp/tools/definitions/graph.rs:691`. Not a plan requirement, but its
   stale "not yet registered" comment caused a false finding in this very audit
   (P1-e). Recommend deletion.

---

## P4 — Plan-by-plan coverage

Six parallel audits partitioned the plan set. Their rows are reproduced with the
evidence they cited. **These were not each re-derived by the auditor**, and two
of their load-bearing claims were refuted on re-check (P1-d, P1-e), so treat
individual rows as leads with citations rather than as settled findings. The
P0/P1/P2 sections above are the verified core.

### Retrieval and code intelligence (PR8–PR10)

| Plan | Requirement | Status | Evidence |
|---|---|---|---|
| 08 sessions/LCM | Compression, supersession, truthful reads | IMPLEMENTED, UNVERIFIED | `src/sessions/lcm/`; `tests/session_suite/lcm_compression/` |
| 09 code index | Deterministic generation identity, exact identifiers | IMPLEMENTED, UNVERIFIED | `tests/code_index_suite/`, `tests/graph_suite/` |
| 10 semantic | Semantic never demotes exact; no cross-project leakage | IMPLEMENTED, UNVERIFIED | `tests/semantic_search_suite/` |
| 15 retrieval lanes | Temporal, task, and diagnostic retriever lanes | NOT IMPLEMENTED | Enum variants exist with no adapter behind them |

### Policy, Git, and runtime surfaces (PR11–PR12)

| Plan | Requirement | Status | Evidence |
|---|---|---|---|
| 11 policy/Git | Receipt-backed transactions | IMPLEMENTED, UNVERIFIED | `tests/pr11_pr12_runtime_acceptance.rs` |
| 11b structure | Surfaces 1–2 | UNREACHABLE | P0-2 |
| 12 root migration | Preflight→backup→cutover→recovery | PARTIAL (PR19 scope) | `src/migrate/`, `src/commands/migrate.rs:74` |
| 12 surfaces | CLI/MCP/HTTP/LSP lifecycle agreement | IMPLEMENTED, UNVERIFIED | `tests/mcp_suite/`, `tests/hooks_lsp_suite/` |

### Hosts, hooks, and feedback (PR13)

| Plan | Requirement | Status | Evidence |
|---|---|---|---|
| 13 provenance | `RetrievalAnchorId`, `EvidenceSpanRecordV1` | IMPLEMENTED, UNVERIFIED | `crates/tracedecay-store/src/evidence_assembly.rs:724`, `:785-838` |
| 13 provenance | GitHub-stack anchor targets | NOT IMPLEMENTED | Plan admits pending at `:27-28` |
| 27/37 hosts | Bounded hooks, async feedback, host conformance | IMPLEMENTED, UNVERIFIED | `tests/agent_suite/`, `tests/hooks_lsp_suite/` |
| 09 doctor | Feedback-family remediation | UNREACHABLE | P0-6 |

### Cross-cutting

| Plan | Requirement | Status | Evidence |
|---|---|---|---|
| 38 §1 branch lifecycle | Branch-DB removal, sweep, `branch gc` | IMPLEMENTED, UNVERIFIED | `src/daemon/git_watch/store_maintenance.rs:109-160`; daily GC `git_watch.rs:992-994` |
| 38 §2 orphan collection | Registry orphan detect/collect | IMPLEMENTED, UNVERIFIED | `store_maintenance.rs:485-549` |
| 38 §3–4 session retention | Generation-scoped windows, one content copy | PARTIAL | P0-7, P1-a |
| 38 §5 incident debris | Quarantine and collection | IMPLEMENTED, UNVERIFIED | `src/retention/incident_debris.rs`; default 30d `src/config.rs:472-474` |
| 38 §6 compaction | Daemon-scheduled incremental vacuum | IMPLEMENTED, UNVERIFIED | `store_maintenance.rs:314-403`; runs only on mounted stores `git_watch.rs:996-998` |
| 38 §7 size observability | Report, soft budgets, Doctor family | IMPLEMENTED, UNVERIFIED | `src/retention/storage_report.rs:34-36` |
| 16 identity | Store keyed on stable repository identity | PARTIAL | P1-c |
| 18 secrets | Sink firewalls on every durable sink | PARTIAL | Heuristic detector `src/memory/hygiene.rs:46`; not a universal ingest firewall |
| 19 convergence | One canonical owner per concern | PARTIAL | Legacy paths remain; plan defers deletion to PR19 |
| 34 refactoring | API-migration planner | NOT IMPLEMENTED | No `ApiMigration` symbol in `src/` |
| 34 refactoring | Rename preview | IMPLEMENTED, UNVERIFIED | `src/mcp/tools/handlers/graph.rs:1326` |

### Count by status

Across the ~60 discrete requirements classified above and in P0–P2:

| Status | Count |
|---|---|
| `IMPLEMENTED BUT UNVERIFIED` | ~34 |
| `PARTIALLY IMPLEMENTED` | ~11 |
| `IMPLEMENTED BUT UNREACHABLE` | 7 |
| `NOT IMPLEMENTED` | 5 |
| `IMPLEMENTED AND VERIFIED` | **0** |

Zero is the honest number for the last row, and it is the single most important
figure in this ledger. `cargo test-all` has never produced a passing run in this
checkout, and the CI `dashboard` job is red at HEAD on `npm run typecheck`
(P0-1). Until one of those changes, nothing in PR8–PR14 can be called verified,
and the large `IMPLEMENTED BUT UNVERIFIED` count should be read as *unknown*
rather than *probably fine*.

---

## Open questions for the owner

1. **Automations action surface (P0-3).** Wire the actions into PR14, or record
   Automations as read-only for PR14 and name the slice that wires them?
2. **Plan 11b Surfaces 1–2 (P0-2).** Build the consumers now, or unregister the
   five endpoints until a surface exists?
3. **Session retention default (P1-a, P0-7).** Three retention engines are
   built, scheduled, and configured off. Is lossless-by-default still correct
   given the storage-size driver Plan 38 cites, and if so, what *does* address
   that driver?
4. **`authority_scope` (P0-1).** Should the backend serve it on
   `DoctorRemediationOperationV1`, or should the frontend stop modelling it?
   The Doctor UI cannot compile either way until this is decided.
5. **`FeedbackRead => false` (P0-6).** Deliberate deferral or oversight? If
   deferral, what condition flips it?

## How to extend this ledger

Add rows only with `file:line` evidence, and prefer the weakest status the
evidence supports. If a requirement looks implemented, the next question is
always *what production path reaches it* — a caller, a registered route, a
declared module, or a gate that can fail.
