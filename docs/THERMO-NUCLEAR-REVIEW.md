# Thermo-Nuclear Code-Quality Review — `codex/tracedecay-total-redesign-plan` vs `master`

> Historical review of one branch snapshot, retained as a record of what the
> reviewers saw at the time. Individual findings have been acted on since and
> are not a current description of the tree; superseded items are annotated
> inline. Re-measure before treating any finding here as live.

Scope: full dirty branch vs master (~780k lines, 2255 files). Reviewed by 16 parallel
Opus reviewers over disjoint subsystem slices. Per-slice detail in
`scratchpad/findings/*.md`. This is a structural/maintainability review, not a behavior sign-off.

## Verdict

**Does not meet the approval bar.** Not because the direction is wrong — the hexagonal
crate split (`domain` / `store` / `application` / `api` / `rusqlite-runtime`), the libsql→rusqlite
migration, and many individual decompositions (definitions/, serve.rs, memory/store.rs,
the policy crate, automation runner, hermes profile_config) are genuinely good work. It
fails on two counts the thermo-nuclear standard treats as presumptive blockers:

1. **~50+ files are over 1000 lines**, many *created* over the line or grown 2–4×, while
   the same branch demonstrates it knows how to decompose. Discipline was applied
   everywhere except the biggest offenders.
2. **Repeated "one step short of the registry" refactors** left whole categories of
   duplication and hazard in place where a single code-judo move would delete them.

Plus a short list of **genuine correctness/safety regressions** that must be fixed before
merge regardless of structure.

Blocker-labeled findings: 10. Total findings: ~90 across 16 slices.

---

## Tier 0 — Correctness / safety (fix before merge; these are behavior-wrong, not just messy)

1. **Un-gated 2,050-line fault-injection runtime shipped in the release binary.**
   `src/application/host_admission.rs:840` — `HostAdmissionTestRuntimeV1` is `#[doc(hidden)]`
   but **not `#[cfg(test)]`**. 114 methods, 83 `_for_test` DB-poisoning/seeding/fault-injection
   helpers compile into the shipped library and expose store-corruption capabilities on the
   public API. Also the single reason host_admission.rs is 3,187 lines.
   → gate behind `#[cfg(any(test, feature = "test-support"))]` in its own file.

2. **`verify_sqlite_integrity` performs no integrity check.**
   `crates/tracedecay-migrate/src/manifest.rs:1525` only opens read-only and closes — no `PRAGMA quick_check`/
   `integrity_check`, reads no rows. It replaced code that did run quick_check + per-row
   checksums. Every migration snapshot is now "verified" purely by being openable; a
   header-valid but truncated/corrupt DB passes. → restore the pragma, or rename to
   `assert_sqlite_openable` so callers stop trusting a guarantee that's gone.

3. **Git operation-state classification has already diverged between two copies.**
   `src/git_index_transactions.rs` classifies `sequencer` as `GitOperationStateV1::Sequencer`;
   `src/git_intelligence.rs` does not. Same repo state, two answers depending on which module
   looks. → extract one shared `operation_state`/`worktree_mode`/`run_git` util; the divergence
   disappears by construction.

4. **Silent identity resolution on the admission path can mint a second project shard.**
   `crates/tracedecay-global-db/src/project_registry.rs:926` (`.ok().flatten()?`) is still wired into
   `host_admission.rs:1157`, `mcp/server/hook_dispatch.rs:25`, `daemon/core_proxy.rs:197`,
   even though fail-closed `try_*` variants were added *specifically* to stop transient DB
   errors from being read as "project not found." → route admission through the `Result`
   path; make best-effort suppression explicit and local at the few callers that want it.

5. **Production config entry points are error-only stubs; the real impl is `#[cfg(test)]`.**
   `src/config.rs:1310-1381` — `ensure/resolve/load_runtime_configuration_for_layout` each
   have a `#[cfg(not(test))]` body that unconditionally errors and a `#[cfg(test)]` body that
   works. Prod callers compile clean, then always fail at runtime; the tests validate a path
   production never runs. → implement for real, or rename the test helpers and delete the stubs.

6. **18 `.expect()` panics on a production execution path.**
   `crates/tracedecay-application/src/feedback/service.rs` — the hand-rolled stage machine
   can't prove "runtime resolved after ResolveRuntime," so 18 runtime `.expect()`s stand in
   for invariants a linear flow would encode structurally. Fixed for free by Tier-2 item #6.

7. **GraphCanvas reducer bug + layout thrash.** `dashboard/src/viz/graph/GraphCanvas.tsx`
   — `nodeReducer` (:167) lacks the `HALO`/`PULSE` guard the event handlers have, so it
   overwrites the synthetic glow nodes' alpha/size/z every frame (bloom renders as opaque
   blobs). And `selectedId`/`onSelect` in the effect deps (:369) tear down Sigma + re-run the
   200-iteration layout on every selection *and every SSE beat*. → add the guard; move
   selection into refs + `renderer.refresh()`.

---

## Tier 1 — Dominant structural regression: god-files, decomposition abandoned at the top end

The branch created or grew past 1000 lines (non-exhaustive), grouped by area:

- **daemon**: daemon.rs 3111→**4377**, service/invocation.rs **3326** (new), lsp_gateway/protocol.rs **2813** (one 2286-line impl)
- **agents**: host_bundle_v2.rs **5004**, agent_cmd.rs →**2299**, context_scout_v2.rs **2046**
- **application crate**: host_admission.rs **3187**, evidence_assembly.rs **2427**, primitives/runtime.rs **2060**, application_surface.rs **1848**, +10 more >1000
- **query**: temporal/ports.rs **4038** (trait buried at 1614), diagnostics_store.rs 1812, diagnostics_query.rs 1495
- **domain crate**: git.rs **2553**, session.rs 1938, observation.rs 1908, configuration.rs 1659, +6 more
- **rusqlite-runtime**: migration_sql.rs **2828**, evidence.rs 1580
- **extraction**: chunks.rs **2768**, artifact_store.rs 2650, fastembed_adapter.rs 1921, session_pool.rs 1778
- **sessions**: cursor.rs 1300→**2512**, kiro.rs 1810, cline_like.rs 1632, transcript_backfill.rs 1456
- **misc**: config.rs 1054→**2615**, git_intelligence.rs 2573, candidate_output.rs 1816
- **db**: connection.rs 670→**2263**
- **tests**: lcm_test.rs **3803**, mcp_suite/support.rs 1851

The tell: `mcp/definitions/`, `serve.rs`, `memory/store.rs`, `policy/*`, `automation/runner/*`
were all split cleanly into submodule dirs — so the pattern and the will exist. The biggest
files just never got the treatment. Remedy is uniform: split each along the seams already
visible in its outline into the sibling module dir; move inline `mod tests` to `tests.rs`
(the tree already does this in `retrieval/exact/tests.rs`). This is pure boundary work, no
behavior change, and it's the single biggest maintainability win available.

---

## Tier 2 — Missed code-judo: refactors that stopped one step short of deleting complexity

1. **MCP dispatch** (`src/mcp/tools/handlers/mod.rs:618`): the flat 114-arm match became a
   10-way `Option`-returning dispatch *chain* — routing now depends on implicit chain order,
   duplicate tool names are silently shadowed, there's no exhaustiveness check, it forced
   **91 new `args.clone()`** (master had 0), and it spawned a test that **scrapes its own
   source text** (`include_str!("mod.rs")` + string-splitting) to reconstruct coverage.
   → go the rest of the way to a `&str → handler` registry with a uniform handler signature;
   all four problems vanish at once.

2. **Sessions provider ingest** (`ingest/project_provider.rs` + `user_provider.rs`): the
   *same* 7-arm provider match is hand-written **twice**, with 25 copy-pasted outcome-wrap
   sites and duplicated cap arithmetic. The `TranscriptSource` trait exists but only unifies
   path *discovery*; the thing that varies (ingest) is hand-dispatched. → one
   `adapter.ingest(ctx)` with `ctx` carrying scope; both dispatchers collapse, ~500 lines
   and both matches deleted, and `transcript_backfill.rs`'s third per-provider parser
   (`derive_timestamp`/`derive_usage`, which will drift from live ingest) folds in too.

3. **Per-host registration** (`crates/tracedecay-agent-hosts/src/agents/{claude,cursor,kimi,...}.rs`): the
   read→parse→identity→pointer→state pipeline is copy-pasted across ~10 host files, bypassing
   the `HostBundleRegistrationInspectorV1` the branch already ships. → one declarative
   descriptor per host driving the existing inspector; deletes ~8 near-identical bodies.

4. **application_surface** (`src/application_surface.rs`): `(operation, request)` carried as
   two parallel representations kept in sync by a 40-line runtime `matches()` table, a
   hand-mirrored 21-entry array, and 5–6 exhaustive matches over the same enum. → make the
   request variant the single source of truth (`operation(&self)` accessor), delete `matches()`
   + guard, derive the operation list.

5. **Validated-newtype macro** (domain crate): the same `#[serde(transparent)]` String-newtype
   macro is re-copied across ~9 modules (~13 copies, >500 dup lines), plus 3 identical digest
   validators and 4 parallel error enums whose divergence is the *root cause* forcing the
   per-module copies. → one crate-level macro parameterized by `$validator`/`$error` + one
   shared `ValueError`.

6. **Feedback cycle stage-machine** (`crates/tracedecay-application/src/feedback/service.rs`,
   2067 LOC): a purely linear, non-suspending async flow is expressed as a `loop`+`match`
   state machine that clones the whole request each iteration, threads data through
   `&mut Option<Progress>`, needs 18 `.expect()`s, and routes to two `FinishPath` variants
   whose handlers are byte-identical. → rewrite as one linear `async fn` with early returns;
   deletes the machinery, the clones, the panics, and one whole finish-path.

Also in this tier: `read.rs` 4 read methods = one 8-step template ×4; `api/http.rs` 9
identical route handlers; store `evidence_assembly.rs` 6 hand-mirrored identity twins;
`connection.rs` `_engine`/plain method pairs with byte-identical bodies (97 call sites, 0 on
master); storage `row_string/row_i64/...` = one generic `row_get<T>` split into six.

---

## Tier 3 — Layer / boundary leaks (logic in the wrong crate)

- **Contract/DTO crate does projector computation.** `tracedecay-store` (`projection.rs`,
  `observation/mod.rs`) runs the projector algorithm and mints authorizations, though
  `lib.rs` says it owns "only persistence contracts and DTOs."
- **Domain crate does policy evaluation.** `configuration.rs:1107` resolves capabilities
  (union denies / intersect allows / deny-wins / expiry) while `lib.rs:3` says the crate
  "performs no policy evaluation." The boundary claim is false — move it or fix the claim.
- **Two-phase-commit coordinator inline in a CLI module.** `src/agent_cmd.rs` (+1809 lines)
  implements `HostComponentSetRegistrationV1` (preflight/stage/apply/verify/commit/rollback)
  in the binary's command dispatcher — can't be reused by the daemon, can't be unit-tested
  without the CLI.
- **Runtime-cache machinery in the config-schema module.** `config.rs` mixes schema/serde
  with process-global statics, a daemon-client registry, store I/O, and a 200-line mutation diff.

---

## Tier 4 — Duplicated security-sensitive / schema scaffolding (drift hazards)

- **Two hand-rolled HMAC-SHA256 cursor authenticators** in the query layer (ports.rs:2030,
  fusion.rs:63) with already-diverging zeroization policy; a third cursor unsigned. → one
  audited `cursor_mac` util.
- **Three byte-budget implementations**, one of which (`git_query.rs:553`) allocates the
  entire serialized payload just to read its length while the streaming version already exists.
- **Two parallel memory-persistence subsystems** (`src/db/memory_v2/**` +7.2k,
  `src/store/memory/**` +14.2k) both defining `repair.rs`/`cutover.rs` with overlapping
  `feedback_history_repair_progress` entry points — establish the ownership boundary before
  they diverge.

---

## Tier 5 — Hygiene

- **PR-plan numbers baked into permanent public names**: `Pr9FallbackSubpayload`,
  `public_pr9_lane_coverage`, `RetrieverKind::PR9_FALLBACK_LANES` — wire-frozen serialized
  contracts with ephemeral PR numbers in stable identifiers. Rename before the format freezes.
- **Pervasive `...V1` suffix noise** on ~every type in `src/agents` (HostKindV1 ×249) with no
  V2 and versioning already enforced elsewhere; forces `as State` aliasing.
- **Tests assert on production source text** (`tests/pr12_production_reachability.rs`
  include_str!s 11 .rs files + asserts byte-offset call ordering) — architecture-by-grep,
  guaranteed to rot on any rename/reorder.
- **Dead code**: `dashboard/codegen` (~1100 lines, output imported by nothing), legacy Zod
  schemas (`data/query/legacy.ts:43-66`), no-op `.replace()` chains, `void nodeRgb`.
  *(Superseded for `dashboard/codegen`: its output is now the live
  `dashboard/src/contracts/` wire boundary, and `npm run contracts:check` is a
  blocking step of the `Dashboard integration` CI job. Do not delete it.)*
- **Mechanical let-chain churn** (~1500 lines across `src/extraction/*_extractor.rs`) folded
  into the feature branch — land as its own commit to keep the diff reviewable.

---

## Recommendation

The architecture is worth keeping; the branch is not mergeable as-is. Suggested order:
1. Fix Tier 0 (correctness/safety) — small, high-value, independent.
2. Take the 6 Tier-2 code-judo moves — they delete the most complexity per unit effort and
   several dissolve Tier-1 god-files as a side effect (feedback service, mcp dispatch, sessions).
3. Sweep Tier-1 decomposition mechanically (mostly move-to-submodule + tests.rs).
4. Tier 3–5 as follow-ups.
