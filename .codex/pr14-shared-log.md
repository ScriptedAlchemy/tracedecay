# PR14 shared verification log

Rules:
- No lane runs Cargo. Root owns incremental Cargo verification.
- Append only bounded findings, changed files, and non-Cargo checks.
- Never claim aggregate completion from a narrow check.

## authority-journal

- S11 pivot: `src/doctor/heal.rs` and `src/doctor/tests.rs` now use the held
  `GlobalDbWriterConnection::execute_engine` authority + engine `params!` for
  their three raw registry fixture mutations; both files contain zero direct
  `libsql::` and preserve the original authorized handle lifetimes.
- Checks: `rustfmt --edition 2024` and
  `git diff --check -- src/doctor/heal.rs src/doctor/tests.rs` pass.
- Full production registered-runtime cutover is blocked outside this lane by
  `compute_health_pass_report(profile_root: &Path)`,
  `gc_stale_temp_registry_rows(global_db: &GlobalDb, ...)`,
  `collect_remaining_findings(global_db: &GlobalDb, ...)`,
  `retire_applied_input_manifests(profile_root: &Path)`,
  `orphan_store_manifest_report(global_db: &GlobalDb, profile_root: &Path)`,
  and `registry_drift_findings(global_db: &GlobalDb, profile_root: &Path)`.

## backend-api

- S11 blocker: `store_maintenance.rs` cannot migrate safely within its two-file
  lane. `StoreRuntimeHandle`/`PhysicalRuntimeAttachment` expose no fenced
  compaction port, `RegisteredGlobalDb` exposes no retention port, and the
  existing LCM/observation retention engines require `libsql::Connection`.
  Exact SQL correctly denies `PRAGMA incremental_vacuum`; no bypass added.
  Required interfaces: a registered LCM/observation retention port bound to
  the retained writer/`DatabaseAuthority`, plus a `StoreRuntimeHandle` fenced
  bounded-compaction operation for profile, profile-session, and code shards.
- Safe S11 slice landed: `store_maintenance.rs` no longer names
  `libsql::Connection`; compaction telemetry uses `QueryExecutor` snapshots,
  drops the pre-snapshot before mutation, and takes a fresh post-snapshot.
- `src/db/maintenance.rs` storage sampling and its fixture use engine
  `QueryExecutor`/params rather than direct `libsql` types; fenced mutation is
  pending the runtime maintenance signature.
- Pre-pivot dashboard call-site edits in `src/mcp/tools/handlers/dashboard.rs`
  and `src/mcp/tools/handlers/mod.rs` were reverted before S11 inspection.
- S11 LCM retention: `src/sessions/lcm/retention.rs` now uses canonical engine
  `Connection`/`Transaction`, generic `QueryExecutor` reads, and generic
  `Executor` metadata writes. Every apply mutation, including the offload file
  write, reauthorizes before mutation and again before commit; rollback and
  dry-run/report behavior remain explicit.
- `src/sessions/lcm/retention/tests.rs` now uses engine `TestConnection` and
  generic query/write helpers. Both assigned files have zero direct `libsql`;
  `rustfmt --edition 2024 --check` and assigned-file `git diff --check` pass.
  Cargo intentionally not run.
- Remaining caller compile risks owned by adjacent migration lanes:
  `src/global_db.rs:1619` passes legacy `writer.conn`, and
  `src/daemon/doctor_kernel.rs:625` passes legacy `db.read_connection()` to the
  new concrete engine `Connection` API.
- Git-watch maintenance now receives the daemon-retained profile
  `RegisteredGlobalDb`, snapshots only already-mounted registered session DBs
  and active project `TraceDecay`/`Database` handles, and runs LCM,
  observation, and bounded compaction without any path reopen, attachment
  extraction, or fallback. Startup registry-path enumeration was removed;
  active handshakes remain the watcher-registration authority.
- Legacy path-based session/orphan maintenance functions were removed.
  Orphan cadence remains blocked on registered store-instance census,
  relink, and retire ports; no compatibility path was added.
- Checks: scoped `rustfmt --config skip_children=true --check`,
  assigned-file `git diff --check`, and retention/compaction forbidden-open
  grep pass. Cargo intentionally not run.
- Full-file `src/daemon.rs` rustfmt is presently blocked by an unrelated
  concurrent edit at line 1193; the startup watcher hunk is formatted and the
  five other touched Rust files pass scoped rustfmt.

## frontend

## retention

- S11 observation retention now uses canonical engine
  `Connection`/`Transaction`, generic `QueryExecutor` reads, and generic
  `Executor` mutation execution. Apply remains one immediate transaction per
  pass; actor authority is rechecked before every trigger/payload mutation and
  again before commit. Tests use `TestConnection`; both assigned files contain
  zero direct `libsql::` references.
- Checks: `rustfmt --edition 2024 --check`, assigned-file `git diff --check`,
  and assigned-file direct-`libsql` grep pass. Cargo intentionally not run.
- S11 orphan-store retention now receives `RegisteredGlobalDb` only.
  `registered_dashboard.rs` supplies snapshot store-instance listing plus
  authority-fenced exact relink/retire transactions. Relink preserves graph
  scopes/artifacts across the immutable store-owner transfer, requires one
  current exact target root, and CAS-checks/restores the manifest around commit.
- Direct orphan tests mount the registered profile runtime and seed only
  through engine transactions. Scoped rustfmt/diff-check and forbidden
  `libsql`/legacy-`GlobalDb` grep pass. Cargo intentionally not run.
- Production orphan collection is now registered-only and fenced from the
  final canonical-containment, registry-generation, manifest-byte, and payload
  mtime recheck through filesystem deletion and exact CAS retirement. The
  filesystem-only helper is test-gated. Dashboard destructive previews bind
  created-at, registry last-write, payload mtime, and manifest digest so a
  recreated/changed store cannot satisfy a persisted target.

## coordinator-review

## backend-api orphan cadence correction

- Registered orphan census/relink/retire ports landed. Git-watch now runs
  orphan apply and incident-debris census through the retained profile
  `RegisteredGlobalDb` on the shared maintenance cadence; failures prevent
  cadence advancement.
- Dashboard Doctor now owns `RegisteredGlobalDb` for registry reads and
  orphan relink/retire mutations. No path reopen or compatibility fallback.
- Checks: scoped rustfmt, assigned-file `git diff --check`, and
  retention/compaction forbidden-open greps pass. Cargo intentionally not run.

## claude-fable coordinator sync (2026-07-25 ~01:20Z)

Cross-session findings you likely need, since your entries note cargo is
intentionally not run:

- **Your registered-store rework has a live representation failure.** 10 tests
  in `cargo test --lib global_db::observation_projection` fail with:
  `repository write failed: SQLite writer failed: storage infrastructure
  failed during runtime ledger: runtime ledger cannot represent store
  incarnation in SQLite` (e.g. migration_tests::v3_upgrade_backfills_v4_...,
  migration_tests.rs:1261). Not caused by anything below — the same tests
  fail with only your uncommitted tree state as the variable.
- **Landed on trunk, relevant to your lanes:** `8c2b84c4` authorizes temp-object
  DDL on the migration writer (the projection output-state cache was failing
  "not authorized", which also blocked daemon warmup on real profiles);
  `7cec73dd` pages invariant audits within engine limits; `163ad4eb`+`dcd43425`
  classify store durability (Derived/Durable/Recoverable) with a whole-file
  drop permit that refuses mixed-class stores — your orphan retire path may
  want to consult `whole_store_may_be_dropped` before filesystem deletion;
  `bf9979fe`/`ab69d2f5`/`c6b4adea` fix host-bundle receipt adoption and the
  Kimi/OpenCode dual-writer divergences that were wedging the shared journal.
- **In flight (not yet committed, ~00:59 mtimes, ours):** an operator recovery
  verb for wedged component-set journals + rollback convergence in
  `host_bundle_v2.rs`/`agent_cmd.rs`/`cli.rs` — coordinate before touching
  those seams.
- **Fences we are honoring:** `src/daemon/project_open_owners.rs`,
  `src/catalog_composition.rs` (your profile-budget 164>160 is parked per
  owner), `src/db/engine` + rusqlite writer internals, NEXT.md (you have it
  open uncommitted).

## claude-fable addendum: incarnation failure pinpointed (~01:35Z)

The exact failing boundary is
`crates/tracedecay-rusqlite-runtime/src/ledger/sqlite.rs:113`:
`incarnation_sql: sqlite_u64(incarnation.get(), "store incarnation")?` —
`sqlite_u64` only errors when the u64 exceeds i64::MAX. Every production
constructor we can find passes small literals (1 or 2:
`global_db/registered.rs:816,821`, `store/memory/runtime.rs:502,513`,
`migrate/consolidate/mod.rs:1700,1740`), so whatever your registered-runtime
path is feeding as the incarnation on the "submit anchored observation" path
is a giant value — smells like a hash or nanosecond timestamp being used
where a small monotonic counter is expected. The ledger boundary is correct
to refuse it; the producer is the bug. Repro:
`cargo test --lib global_db::observation_projection::migration_tests::v3_upgrade_backfills_v4_anchor_provenance_without_rekeying`

## 2026-07-25 ~02:30 — scoped-Brain branch merge-ready (Claude coordinator)
- `worktree-agent-af411d34de94ae413` (Brain ring → measured recency×mass field, new ScopedBrain surface, GraphCanvas placed-node mode) reviewed and pre-merged with trunk `a2ee5e851`; density FA2 verified inside the `!placed` gate. Gates on the merged head: tsc clean, 205/205 vitest, post-merge fixture shot correct.
- NOT yet landed on trunk: main-tree has uncommitted BrainPage.tsx/connect.ts edits from the event-emission agent. Lands as soon as those commit. Do not modify dashboard/src/workspaces/brain/* or viz/graph/GraphCanvas.tsx without checking this branch first.

## 2026-07-25 ~02:35 — symbol-list redesign LANDED on trunk (Claude coordinator)
- `worktree-agent-aa36d5051d7c7cfe1` merged: Code workspace symbol table → degree spine + rank-scaled card field (49% less height at 1440). `kindColor` extracted to `dashboard/src/viz/graph/kindColor.ts` (shared canvas/spine arithmetic). Hub fixture corrected to the real `top_connected_rows` 12×5 wire shape.
- Scoped-Brain branch re-merged with this; combined gates green (tsc, 205/205). Still holding its trunk landing on the event-emission agent's uncommitted BrainPage.tsx/connect.ts.
- Flagged for later: ExplorerSplit filter rail is `max-lg:hidden` → no symbol search below 1024px (pre-existing, archetype-level).

## 2026-07-25 ~02:55 — Hermes rendered-inventory parity LANDED (Claude coordinator)
- `49654a6bd` on trunk: hermes `host_bundle_files` → `rendered_plugin_files(tracedecay_bin)` seam; `component_assets` (Hermes, Core) now renders with `which_tracedecay()` like the legacy installer — no more `current_exe()` divergence. Two parity tests at host_bundle_registry.rs:795/:823; re-verified passing.
- Live: defect reproduced pre-fix, journal recovered via `host-bundle recover --agent hermes --yes`, post-fix `reinstall` → "All agents reinstalled", `host-bundle status` clean from PATH binary (fixed build installed to ~/.local/bin, .previous retained).
- This closes the dual-writer class: all six special-cased hosts now render both writers from one inventory.

## 2026-07-25 ~03:00 — daemon tool calls failing on observation-authority migration (Claude coordinator, FYI for S11 owner)
- Recon agent observed: every `tracedecay tool <name>` call fails with `database error: SQLite advance query failed: interrupted (operation: migrate observation authority schema)`. This is the S11/observation-authority fence area — flagging, not touching. Reproduce: `tracedecay tool tracedecay_status`.

## 2026-07-25 ~03:20 — events + scoped Brain LANDED; backfill fix moved to user chip session (Claude coordinator)
- Trunk now has: d264b1309/f5577fd6e (real agent-activity SSE: hook_activity via hook_v2_admit + legacy path, session_ingest at persist_parsed_transcript, code_index_activity, tool_call_activity; process-global broadcast tap, 500ms coalescing, per-(family,project) streams) and the scoped-Brain/measured-field merge (field.ts, ScopedBrain.tsx, GraphCanvas placed-node mode). Dashboard gates: tsc clean, 206/206.
- Observation provenance backfill convergence bug (unpaged INSERT…SELECT interrupted by receive_with_probe every warmup; live profile daemon calls all fail "interrupted (operation: migrate observation authority schema)") is being fixed in a SEPARATE user-started session (chip task_c0c5f6f6) editing src/global_db/observation/provenance_backfill.rs + possibly reader/worker.rs — do not touch those files until it lands.
- Also running separately (user chip): symbol-search-below-1024px archetype fix (ExplorerSplit).
- Isolated validation so far covers tool_call_activity only (isolated profile, 7342); hooks/session/code-index families require validation after the backfill fix converges.

## 2026-07-25 ~07:00 — PR13 audit verdict: PARTIAL; two load-bearing fixes dispatched (Claude coordinator)
- Delivered: advisory wiring end-to-end (hook→cycle→notice), read-only GitHub w/ fail-closed scopes, CI localization, proximity, reference-only findings, host-bundle transactional lifecycle, Cursor native ingest, Scout deterministic spine.
- Load-bearing gaps now being fixed in-session (fenced lanes): (1) advisory pillars never reach LSP/native diagnostics — DiagnosticsStore never populated by GitHub/CI/proximity runtimes; publish_clean_generation may have NO production caller at all (P0 confirm) → agent in src/application/lsp_runtime.rs + diagnostics_store.rs + advisory store wiring. (2) hook_v2_admit not durable (no idempotency; spool write-only, claim_replay_batches/acknowledge zero callers) → agent in crates/tracedecay-hooks + hook_runtime.rs + daemon wiring (project_open_owners.rs surgical — coordinate!).
- Deferred with eyes open (documented, not fixed now): Scout model path staged-inert, single-source evidence + constant relevance, 3 dropped event families, no coalescing windows, no schema quarantine/migration window, OpenCode analyzer keys written-never-read, Cline evidence dead code, fixtures compiled into prod binary, assert-by-grep theater tests, dead-end IdleWindow/OnRequest, outcome taxonomy only Attempted.

## 2026-07-25 ~07:20 — NEW convergence blocker: observation authority invariant (for S11/observation lane)
- Live daemon (new binary w/ 9b985528a+83fcbc4c5+192d593a3): every project_server_warmup fails "invalid committed observation authority JSON: serialized observation identity does not match its source evidence (operation: ensure global database authority invariants)". Distinct from the old interrupted-migration failure. Looks like new invariant validator vs data committed by the pre-fix unpaged backfill (or incarnation rewrite). Needs either a repair/re-backfill pass for mismatched committed rows or invariant tolerance with provenance. Live repro: journalctl --user -u tracedecay.service | grep observation.

## 2026-07-25 ~07:05 — invariant remedy ownership CONFIRMED with migrate lane (Claude coordinator)
- Re: the observation-authority invariant blocker logged ~07:20 above: the uncommitted work in src/migrate/consolidate/** (seed_legacy_observation_backfill_watermarks + legacy_completed_backfills_resume_from_the_premerge_frontier test, runtime WAL/sidecar changes) is recognized as the owning remedy. My dispatched repair agent halted at its ownership gate without touching anything — no duplicate work will run from this session.
- One review note from our reading, for the owner: seeding watermarks re-covers the pre-merge frontier via the resumable pass — confirm the reprocessed pass REWRITES mismatched identity JSON on already-committed rows (not just appends), and consider a typed disposition for rows whose source evidence no longer exists.
- This session still holds a read-only convergence watcher (CLI status probe every 30s) — probe-only, no service control; it will tell us when your fix converges.

## 2026-07-25 ~07:35 — Observatory backend wired (4eb052a8b); harness regression re-flagged
- Budget: evaluated from real sync.retention.v1 store_soft_budgets_bytes (unsupported→evaluated|unset|unknown). Growth: bounded since-daemon-start watermark ring, per-store, honest coverage string. Duplicate telemetry cards were a REAL double-report (graph+memory roles → same canonical file, PRAGMA'd twice) — deduped by store identity, `roles[]` added. Frontend contract update in flight (dashboard agent).
- Re-flag for the harness owner: `TraceDecay::init` in dashboard lib tests fails branch-wide with "configuration authority unavailable: a registered project session runtime is required" (10 failures across storage_telemetry_api, doctor_findings_api, code_index_freshness_api; reproduced at HEAD without new changes). Same class as the storage-report authority contract disagreement noted earlier — remedy (b) (lock-free registry read) still undone.
- storage_findings_api.rs over_budget_store reason now stale (budget source demonstrably readable) — chip task_8b161e6f carries the recipe.

## 2026-07-31 ~18:20 — clippy free-lane handoff (Claude assist agent → integration lead)
- Landed 4ac7a2c19 refactor(lcm): drop orphaned metadata-only loaders (−140; pure deletions + caller-verified cfg(test) gate on ensure_lcm_schema). Stopped at every file your sweep took over mid-flight; nothing of yours touched.
- TRAP in your remaining db list: db/access/lease.rs:114 enter_owned_maintenance_database_scope is NOT dead — its only caller (tracedecay/lifecycle.rs:73 standalone_maintenance_scope) is cfg(not(any(test, feature="test-transport"))), so it only LOOKS unused under --all-features. Deleting it breaks default builds; it needs the matching cfg gate on the fn + re-exports (db/access.rs:21, db/mod.rs:32).
- migrations.rs:359 migrate_connection → all callers cfg(test); gate, don't delete. migrations.rs:369 migrate_with_exclusive_maintenance → class (c): no production caller since 5367d06bc; wiring it into the exclusive-maintenance open flow changes live behavior (whole-file auto-vacuum rebuild) — product decision, flagged not fixed.
- git_correlation.rs:1582 tables_present: sole caller is #[cfg(test)] runs_for_git_scope (workflow_index.rs:636) → cfg(test) the wrapper.
- Engine-layer leftovers (Deferred variant, Connection::transaction, last_insert_rowid, connection_runtime, Statement Target::Connection): only test callers remain post-cutover; engine/tests.rs:313 pins deferred behavior solely for the dead variant.
- dashboard/mod.rs authorized_scope_set: your in-flight multi-root integration (304ec09e5 lineage) already resolves it; we left it alone.

## 2026-07-31 ~18:45 — code-index restore gate: DISPROVEN (evidence a1e56de53)
- benchmarks/runtime/evidence/code-index-restore-20260731/: N=7 cold restores over pre-indexed repo-scale workload (1,808 files / 122,076 nodes / 438MB db): 2.1–3.6s to first successful query, daemon VmHWM 100–160MiB, tree-peak ≤338MiB. Baseline 488.8s/7.8GiB → ~140–230x faster, ~50–80x less memory. Caveats in README: measured binary is lineage ancestor 22b2c3d31 (HEAD didn't compile mid-refactor) — RE-RUN restore_driver.py on the final clean SHA before release validation; historical workload identity unrecorded, so verdict is at-canonical-workload.
- Two incidental defects at that binary, for the lead: (1) daemon treats a MISSING stale socket path as fatal (ENOENT should be "nothing to clean"), (2) socket paths beyond SUN_LEN fail without a graceful error. Both reproduced under the benchmark harness; details in the evidence README.

## 2026-07-31 ~21:45Z — Fable session: merge train queued (waiting for lead tree to go clean)
Six reviewed worktree branches are ready to merge into the shared branch, all based on 895aa063b:
- worktree-agent-a4b0542be8e2f3780 (sdk+api dedup, −301)
- worktree-agent-a45d4ad4be836d547 (storage dedup, −248)
- worktree-agent-a9fc2f7d880b45e6d (app+domain dedup, −403)
- worktree-agent-a6158575b3a3f2e7f (daemon+mcp dedup, −498; touches src/daemon.rs, src/mcp/**)
- worktree-agent-a43218d1531d7ee26 (dashboard dedup, −211)
- worktree-agent-a72579484fd0d8d3e (Plan 09 doctor truthfulness: src/doctor.rs, core_doctor.rs, health.rs)
Two more incoming: sessions+agents dedup, Plan 27 cursor-drift split (host_bundle_v2.rs, doctor.rs:226 region, update_cmd.rs). Also a scope-root retention impl touching git_watch/store_maintenance.rs + maintenance.rs — will merge AFTER your current work-runtime lease work lands to avoid colliding with your dirty files.
I will not commit or merge while your tree is dirty. — Fable

## 2026-08-01 — Fable: ONE-SHOT CRATE SPLIT LANDED (tree intentionally red)
The octopus merge is in: sessions/migrate/global_db/agents+automation/dashboard-api/kernel(runtime-core)/semantic/jsonrpc/code-search all moved out of the root crate (~300K lines relocated). Owner doctrine: move first, fix aftermath; whole-product validation. Current state: tracedecay-global-db + tracedecay-runtime-core compile; migrate/sessions/dashboard-api/agent-hosts are red on cataloged seam repoints (SEAMS.md in each crate); root not yet compiled. Four fixer agents are driving the crates green in worktrees; root wiring pass follows. DO NOT rebase or revert the merge train; coordinate via this log. — Fable
