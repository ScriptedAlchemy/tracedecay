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

The observed fragmentation (444 store directories, ~101 GB against ~37 real
repositories) is runtime data that this audit could not inspect under the
freeze, so it is **not** classified here. It should be treated as a
reconciliation task against existing Doctor relink/retirement machinery
(`src/global_db/registered_dashboard.rs:222-360`), not as evidence that the
current identity code is wrong.

---

## P2 — Plan text that is now wrong

| Plan | Line | Says | Should say |
|---|---|---|---|
| `11b-structure-visualization.md` | 77-79 | "register the existing `get_call_chain`" as pending work | Registered at `src/dashboard/mod.rs:1013`; the open work is the missing frontend surface (P0-2) |
| `00-plan-set-index.md` | 470-472 | Loom time boundaries listed among implemented checkpoint items | Accurate, but must note its only backend test is undeclared (P0-4) and cannot contribute to verification |
| `00-plan-set-index.md` | 474-478 | Checkpoint is "implemented but unverified" pending `dashboard_api_test` | Correct and should be kept. Add that the suite currently cannot pass because the CI `dashboard` job is red on `npm run typecheck` (P0-1) |
| `NEXT.md` | 140-159 | Plan 38 fully delivered including "§3 session retention" | Delivered as mechanism; inert by default. State the default explicitly (P1-a) |

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

---

## Open questions for the owner

1. **Automations action surface (P0-3).** Wire the actions into PR14, or record
   Automations as read-only for PR14 and name the slice that wires them?
2. **Plan 11b Surfaces 1–2 (P0-2).** Build the consumers now, or unregister the
   five endpoints until a surface exists?
3. **Session retention default (P1-a).** Is lossless-by-default still correct
   given the storage-size driver Plan 38 cites?

## How to extend this ledger

Add rows only with `file:line` evidence, and prefer the weakest status the
evidence supports. If a requirement looks implemented, the next question is
always *what production path reaches it* — a caller, a registered route, a
declared module, or a gate that can fail.
