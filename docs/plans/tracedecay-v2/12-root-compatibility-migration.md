# TraceDecay V2 Root Compatibility and Migration Implementation Plan

**Goal:** Turn the existing `tracedecay` package into the stable binary, daemon, composition root, host installer, and bounded migration shell for the V2 crates; preserve inventoried current behavior until its explicit cutover, then expose only current V2 bindings while each context backfills, validates, rolls back data safely, and retires V1 without a flag day.

**Architecture:** The published root package wires immutable V2 crate contracts into process-specific service graphs. A generated migration inventory owns every legacy surface. Typed route modes select one effect owner per bounded context, V1 adapters exist only while that context is pre-cutover or during an explicit operator rollback, shadow comparators use frozen vector watermarks, and signed cutover receipts make data migration reversible. There is no live fallback for stale clients, protocols, plugins, or tool names. The root never becomes a second application layer.

**Tech Stack:** Rust 2024 workspace; existing `tracedecay` binary/package; V2 workspace crates/surfaces from plans 01–28; Clap; Tokio; Axum; JSON-RPC/MCP; generated OpenAPI/SDK, capability, and host-bundle catalogs; SQLite V1 readers plus `tracedecay-store`; current provider manifests/installers; Cargo nextest; release-plz; crates.io and native host-marketplace coordinated publication.

---

## 1. Contract Lock and Relationship to the Plan Set

This document is the execution plan for the “Existing root crate” row in master-plan Section 22. It does not replace the crate plans:

- Plans 01–08 own domain, store, capture, projector, query, policy, hook, and capability behavior.
- Plan 09 owns transport-neutral reads, commands, jobs, replay labs, audit, and authorization.
- Plans 20–24 add the generated configuration plane, unified CLI/MCP/output contract, incremental context scout, temporal session/LCM retrieval, and canonical task/plan/multi-agent executor. Root composition wires their adapters, exactly one scheduler/lease authority, migrations, cutovers, and deletion receipts; it owns none of their semantics.
- Plan 10 owns HTTP V2, SSE, authentication, OpenAPI, and generated clients.
- Plan 11 owns the new frontend and legacy-dashboard view/action parity.
- Plans 13–24 own durable research anchors, historical regressions, retrieval evaluation, cross-project scope, the official public API/SDK declaration, mandatory sanitizer/privacy, unified configuration, exhaustive CLI/MCP/output, incremental context scouting, temporal session/LCM retrieval, and canonical task/plan/multi-agent execution. Root composes their ports, lifecycle, packages, and cutovers; it does not fork their contracts.
- This plan owns only root composition, V1 compatibility adapters, process lifecycle, CLI/MCP/legacy HTTP bindings, provider installation, upgrade/release, migration orchestration, and final V1 code/store retirement.
- Plan 20 owns configuration registry/resolver/history/impact semantics. Root inventories and imports every V1 file, flag, environment read, provider/hook/daemon setting, and dashboard mutation, then deletes all live legacy readers after generated config bindings pass parity.
- Plan 21 owns binding/output semantics. Root supplies thin generated Clap/MCP adapters, stdout/stderr/exit and protocol framing, then deletes handwritten schemas/dispatch allowlists/renderers/aliases after parity; it cannot keep a root-local catalog or error model.
- Plan 22 owns scout workflow/model-gateway/host-delivery semantics. Root supplies daemon scheduling, optional App Server adapter, and host handshake wiring without putting model/search work on the hook path.
- Plan 23 owns temporal message/LCM search and context semantics. Root supplies V1 import/shadow adapters and generated bindings, then deletes independent legacy FTS/LCM/ranking/load-routing paths after cutover.
- Plan 25 owns `tracedecay-code-index`. The canonical chain is daemon watcher/manual/config trigger → root `SchedulerKernelV1` → plan-03 sanitized snapshot observation → plan-04 projector `CodeIndexBuildRequestV1` → root `src/v2_adapters/code_index.rs` → plan-25 builder. Neither watcher, capture, projectors, nor application imports/calls the production indexer or creates a second intake lane.
- Plan 26 owns accounting/observability contracts, projections, metric registry, and SLO semantics. Root supplies process telemetry sources and transport bindings only.
- Plan 27 owns the cross-host package/component contract, host capability and difference vocabulary, generated bundle semantics, conformance matrix, and native marketplace release shape. Root supplies the privileged deploy/publish adapter, preserves foreign host state, and consumes only PR 36R-signed release output built from the catalog compiler's unsigned payload; it cannot maintain another skill, hook, role, MCP, or installer manifest.

Program numbering remains authoritative from the master plan. This document refines existing numbers with suffixes; it does not insert or renumber the domain PRs:

| Master slice | Root slice in this plan |
|---|---|
| PR 3 | PR 3R compatibility inventory and baseline ledger |
| PR 4A | PR 4AR read-only V1-backed workbench adapter |
| PR 7F | PR 7FR source-family shadow capture composition |
| PR 12A | PR 12AR first end-to-end composition slice |
| PR 17–23 | Root companion bindings only; domain behavior remains in the named crate |
| PR 24A–24E | Application/API/query transport foundations, PR 24E0 service-isolation foundation, and PR 24E1–24E8 root adapters |
| PR 24F | PR 24FR host/daemon hook compatibility wiring after PR 24A establishes `HookApplicationPort` |
| PR 24G/24H | Root scope/agent ergonomics and privacy CLI/MCP/API/lifecycle bindings over the same application/catalog contracts |
| PR 25 | PR 25R API/router/static-shell coexistence |
| PR 33 | PR 33R whole-profile migration controller (root orchestration/receipts beside plan 02's PR 33S/33S-2 store importer executor) |
| PR 34 | PR 34R parity runner and operator dashboard binding |
| PR 35 | PR 35A–35J bounded-context routing cutovers |
| PR 36 | PR 36R V2-default release and V1 data archive window |
| PR 37 | PR 37A–37L V1 code/store/dashboard/host-installer/path-routing retirement |

No V2 crate may import the root crate. Root adapters may import both V1 modules and public V2 contracts. A compatibility adapter cannot contain policy, query planning, projection, business mutation, or SQL beyond invoking a V1 repository that remains authoritative for that route.

## 2. Goals

- Keep `tracedecay` as the one user/host-invoked integration command and `tracedecayd` as the one private service-manager-owned authority binary throughout the rewrite; their exact build-set/protocol handshake is mandatory, and incompatible running processes restart rather than degrade.
- Resolve profile/privacy domain, configuration, runtime identity, and route/catalog generation once at process bootstrap. Preserve invocation CWD/host facts in `RequestContext`; each request's exact domain `ScopeSelectorV2` is authorized/resolved by the application service, so a long-lived process never pins the first project/worktree/branch as global scope.
- Give each process only the services it needs: hooks and one-shot clients never open stores; the daemon owns every ordinary live V2 writer/read pool/query/backup/checkpoint worker and exposes the typed local/HTTP application service.
- Inventory every CLI command/flag/output, MCP tool/schema/renderer, HTTP route/action, dashboard behavior, config/env/default, hook/provider event, database/sidecar, installer mutation, doctor/repair, and release behavior before moving it.
- Preserve V1 external behavior only while its bounded context is V1-authoritative or in non-effecting shadow; at cutover, current clients switch atomically to V2 and stale clients fail closed. Keep V1 stores read-only for one full release solely for verified data rollback/archive.
- Ensure one canonical effect owner at all times. Shadow execution may compare results but may not inject a second hint, advance a second source head, apply a second mutation, or launch a second automation run.
- Backfill from read-only V1 sources, compare at fixed watermarks, cut over by bounded context, and roll back by route/lease changes without deleting V2 evidence.
- Make migration resumable, private, hash-manifested, observable, and explicit about every input's disposition — `retained`, `skipped` (with reason `corrupt`, `ambiguous`, or `unavailable`), `quarantined`, `redacted`, or `deleted` — using the Section 14.1 per-entity manifest schema.
- Split `src/global_db.rs`, `src/mcp/server.rs`, `src/mcp/tools/definitions.rs`, `src/sessions/lcm/query.rs`, `src/agents/mod.rs`, and other giant modules by deleting migrated responsibilities, not by copying them into new monoliths.
- Preserve agent host install/update/uninstall semantics, safe config merge/backups, plugin manifests, daemon service management, self-update, release assets, and package installation.
- Publish one signed host release set whose canonical `HostIntegrationManifestV1`, generated `HostBundleManifestV1`, resolved `ResolvedHostBundleV1`, package/component digests, capability/difference/conformance reports, SBOM/license inventory, supported-host matrix, and marketplace locators all bind the same source commit and catalog digest.
- Make an older binary, running daemon/MCP process, plugin protocol, or stale tool name fail before service/store use with an exact restart/update/current-replacement error; it must never downgrade, silently fall back, or corrupt a store.
- Publish privacy-safe advisory `AgentPresence`, `WorkClaim`, acknowledgement, handoff, and release evidence from every supported host/worktree, with stable session/agent/goal/retrieval anchors and bounded TTL; never mirror full prompts or turn coordination into a lock.

## 3. Non-goals

- No business rules, canonical IDs, query AST, ranking, projection semantics, policy VM, SQL schema, HTTP envelope, or UI state in root composition.
- No flag-day switch, writable V1 importer, bidirectional database replication, distributed transaction, or implicit cross-profile merge.
- No irreversible representative-message dedupe. PR #410 views remain query-time/projection semantics over complete sanitized native observations, lossless for retained non-secret structure/semantics.
- No resurrection of Hermes-specific runtime profiles, bridges, config projections, pending-skill inventories, or dashboard ownership removed by PR #407.
- No automatic deletion of legacy stores during upgrade, migration, V2 defaulting, rollback-window closure, or uninstall.
- No inference that `tracedecay.db` is disposable code-index cache. It currently contains graph-resident durable memory/fact tables; code-graph parity alone can never authorize deletion.
- No remote GitHub writes from the first V2 investigation product. Existing Git/PR reads and local semantic tools remain available with explicit freshness.
- No public stability promise for internal V2 implementation crates beyond what the root package requires; their exact-version release contract is specified in Section 16.
- No live compatibility fallback for old running MCP/daemon/plugin clients, stale protocol/catalog generations, retired tool names, or renamed arguments. Required data migration and read-only archive rollback are separate operator workflows in the current binary.
- No distributed work scheduler or mandatory claim enforcement. Presence and claims help agents coordinate and can be acknowledged/handed off, but they never authorize, deny, reserve, or prove causation.

## 4. Refreshed Master and Incoming-Change Baseline

The implementation lead must run this live-state refresh before every root slice:

```bash
git fetch origin --prune
git rev-parse HEAD origin/master
git log --oneline --decorate -5 origin/master
gh pr list --state open --json number,title,headRefName,baseRefName,isDraft,mergeable,updatedAt
tracedecay tool branch_list --args '{"project_path":"<checkout>"}'
tracedecay tool pr_context --args '{"branch":"<each-open-head>","project_path":"<checkout>"}'
```

Record local commit, remote head, merge base, fetched-at time, TraceDecay index watermark, direct changed-file set, structural impact set, and any disagreement. GitHub is authoritative for live PR/check/merge state; TraceDecay is authoritative for indexed semantic context only at its named commit/watermark.

The master plan §2.6 and [plan 13](13-research-provenance-and-context-anchors.md) are the sole versioned publication snapshot. Before every execution slice, fetch current master/open PRs, classify new inputs, rebase the implementation branch, and regenerate actual crate/schema/protocol/tool inventories. The checked-in [research provenance and context anchors](13-research-provenance-and-context-anchors.md) and [historical failure regression matrix](14-historical-failure-regression-matrix.md) are normative planning inputs; every root execution receipt cites their refreshed successors and stable `FM-###` IDs.

The 2026-07-09 planning baseline is:

| Change | State observed during planning | Root migration consequence |
|---|---|---|
| PR #405, `fix(storage): adopt legacy identity stores safely` | Merged into the accepted base; three files, including `src/storage.rs`, `src/tracedecay/lifecycle.rs`, and resolver tests | Treat its repository identity markers, linked/detached worktree handling, candidate inventories, adoption/retirement receipts, and split-identity conflicts as canonical V1 import evidence. Never recreate the pre-#405 resolver in a migration adapter. |
| PR #412, `fix(runtime): drain daemon safely during upgrades` | Merged into the accepted base; adds `src/lifecycle_lease.rs` and changes daemon, service, watcher, MCP, update, and doctor lifecycle | Preserve shared/exclusive lease, drain, writer quiescence, checkpoint ordering, and service-state semantics. Retain inherited-token behavior only as a V1 differential; V2 requires fresh OS-lock acquisition and epoch CAS for every mutating process. |
| PR #407, `fix(hermes): use the user TraceDecay profile` | Merged in publication base `78bfbfbc`; broad migration/removal and bounded `src/migrate/hermes.rs` compatibility importer, extended by #443 | Accepted base. Root composition has one user profile. Removed Hermes bridges/config/inventory modules remain historical inventory/import rows, never new V2 dependencies. The current importer is bounded V1 read-only compatibility behavior, not permission to restore a Hermes data profile. Preserve facts/session migration and collision/unresolved receipts. |
| PR #410, `fix(sessions): collapse copied subagent prompts` | Merged | Freeze its `direct_user`, `subagent`, `tool_result`, sanitized-native, and parent-representative behavior across CLI/MCP/LCM/message search. Root only maps bindings; domain/projector/query own semantics. |
| PR #411, foreign-installation doctor authority | Merged | Inventory one ownership predicate and remediation. Foreign packages are information/preserved state, never update/delete targets. |
| PRs #414/#419, `tracedecay_move_symbol` and race-safe writes | Merged | Inventory the historical dry-run/default execution and rollback evidence, impact report, exact source/destination versions, symlink/same-file/hard-link rejection, atomic sibling renames, last-moment revalidation, conflicts, reindex, and every binding; map V2 to operation-specific edit inspect/commit/recovery before adapter cutover. |
| PR #415, release-PR integrity | Merged | Preserve trusted-base changed-file allowlist, tracked-ignored-file guard, and clean-worktree enforcement; V2 extends it across generated catalog/schema/API/SDK/dashboard/release inventories. |
| PR #417, doctor identity-split visibility | Merged | Preserve error-aware split inventory and byte-unchanged candidates; status/doctor must not turn identity conflict into absent index or offer initialization. |
| PRs #413/#416/#418/#427/#429/#431/#433, releases v0.0.46 through v0.0.52 | All merged; v0.0.52 tag `09080e80`, publication head later advanced through #438 to `3bea5ec7` | Packaging/version baselines only. Refresh version, Cargo metadata, release manifest, and checks; no architectural dependency on release PR contents or inference that an earlier 0.0.47 planning probe was upgraded. |
| PR #420, early daemon proxy/hot swap | Merged | Root chooses proxy/local authority before opening stores, reconnects reads/current calls per request, never replays an uncertain write, and distinguishes safe reconnect from incompatible new host session. Merged #422 adds compatible generation-scoped `tools.listChanged` refresh. |
| PR #425, explicit split-store consolidation | Merged as `de3d05dc`; final head `d3bb28b5` | Preserve its historical V1 planning/execution boundary (commands currently named plan/apply), canonical macOS/Linux/Windows paths (`12182510`), final path-plus-file/inode holder identity, source freeze, reservations, dual backups, deterministic confirmation, restartable ledger/staging, table merge/rebuild/reject/collision dispositions, remapped LCM edges (`82cfa9b9`), exhaustive verification, marker/registry cutover, and doctor recovery. Map it to operation-specific V2 consolidation inspect/plan/start and treat it as the accepted anti-corruption seam, not a second canonical merger. |
| PR #426, untracked branch graph recovery | Merged as `96dcedac`; head `6c935e77` | Inventory graph artifacts by verified file identity/fingerprint even when metadata is absent; preserve unmatched branch databases, reconstruct metadata only after proof, and prevent GC or consolidation from discarding the sole branch graph copy. |
| PR #428, divergent session variants | Merged as `00612894`; head `a9b4f16c` | Compare same-ID sessions by canonical content/provenance. Dedupe only exact duplicates; assign stable variant identities to divergent histories and remap every message, LCM, summary, and source-edge dependency. |
| PR #430, indexed consolidation-family lookup | Merged as `cc95929c`; head `49acde38` | Materialize normalized indexed lookup tables for consolidation families, verify production SQL plans, prohibit recursive JSON/source rescans in hot loops, and make index construction resumable and bounded. |
| PR #432, hook lifecycle quiescence | Merged as `22497aa7`; heads `302ce64f`/`b2fd149f` | Every hook acquires the profile lifecycle lease before config/startup/store work, drains provider input when an exclusive owner is active, and performs no agent/plugin installation or local-store fallback during quiescence. |
| PR #434, conflict-safe registry reconstruction | Merged as `effc146b` | Classify manifest eligibility, refuse path/alias ownership theft or stale/retired resurrection, reconstruct transactionally under lifecycle ownership, retry idempotently, and expose blocked proof through doctor. |
| PR #435, FTS repair outside search reads | Merged as `4f0d1b42` | Keep every search/query path side-effect free, distinguish FTS-only damage from whole-database corruption, return typed degraded coverage, and route repair through a fenced maintenance command with verification receipts. |
| PR #436, graph mmap disabled across peer checkpoints | Merged as `accc79f0` | Configure peer-opened graph connections with `mmap_size=0` until immutable generations make mapping safe; retain mixed-page-size and peer-checkpoint regressions in store cutover gates. |
| PRs #437/#442/#444/#446/#449/#451, releases v0.0.53 through v0.0.58 | Latest accepted release merged as `81fe404c` after #452 | Publication-only accepted inputs. Record package/tag/catalog/schema digests and checks; do not infer an installed runtime or architectural change from release state. |
| PR #438, restart-safe applied-manifest retirement | Merged as `3bea5ec7`; final head `4f7b2b2c` | Import the accepted contract: exclusive lifecycle capability, proof of legacy ownership, transactional retirement of schema-2 `Applied` source/target manifests, original shard data retained, destination canonical, idempotent retry, and fail-closed doctor evidence. |
| PR #439, derive orphan stores from registry reconstruction | Merged as `974d423b`; final head `de55e376` | Use the authoritative read-only per-manifest registry reconstruction diff for doctor orphan populations; do not retain token-accounting or path-proxy counters. |
| PR #440, isolate registry reconstruction conflicts | Merged as `0dd1fd7d`; final head `7a56db8e` | Preflight every eligible manifest independently so one conflict remains visible without suppressing unrelated missing rows; migration/doctor share the same per-manifest dispositions. |
| PR #441, `fix(hermes): route memory and context safely` | Merged as `a1de60b8` | Preserve session-workspace routing, project-selector propagation, bounded first-turn guidance, context-clone isolation, one TraceDecay profile across all Hermes host profiles, and WAL/checkpoint snapshot-race evidence as FM-138–FM-146 fixtures. Import the shipped user compatibility stores once; do not copy adapter-local regex, process-CWD fallback, generalized response-handle dereference, a second permanent user-memory authority, live-main-file copy/reflink, or direct client SQLite access into V2. |
| PR #443, `fix(agents): recover post-update integration state` | Merged as `fcc92afd` | Preserve exact owned-block recovery, ambiguous-marker fail-closed behavior, per-session legacy destination proof, idempotent user-session import, unresolved-memory preservation, and the distinction between nonblocking automatic-reinstall warnings and blocking integrity/copy/identity/partial failures. Explicit migration remains strict. V2 imports these as FM-151–FM-152 fixtures, not as provider-local mutation engines. |
| PR #445, `fix(hermes): isolate projectless host routing` | Merged as `49bc0805` | Preserve installed/configured host-profile ownership, per-session provider-home reset, Hermes-home project exclusion, registered descendant-project routing, explicit user-scope project bypass, and read-only-selector versus mutating-route separation. V2 moves those rules into canonical scope/application/catalog generation and tests fresh install plus 0.0.55→0.0.56 update/reinstall; #445 adds no schema migration. |
| PR #447, catch-up and integrity hardening | Merged as `c86952cd` | Preserve FM-153/FM-154 and newly exposed FM-158/FM-159 differential fixtures: provider-scoped scan-once catch-up, concurrent-request coalescing, semantic-frame-safe chunking, generation-aware cursors, exact concrete-store recovery, and checkpoint result classification. V2 does not port handler-static singleflight, multi-project transcript copies, branch databases, dual `.dirty`/`.sync.lock` sidecars, forced TRUNCATE checkpoints, or installer-owned hook trust. |
| PR #448, user message scope and daemon shutdown | Merged as `2e06272d` | Preserve selected-profile routing, provider-ambiguous session refusal, live-hook priority, registry/source failure, and descendant-process shutdown fixtures. V2 replaces query-triggered catch-up, process-local coalescing, handler DB opens, and component-local child registries with explicit operations and one daemon-owned authority. |
| PR #450, secure lifecycle handoff and Windows migration recovery | Merged as `3b9e42bb`; final head `6a33ffe4` | Preserve FM-095/FM-160 fixtures: diagnostic owner text never grants Windows exclusivity; post-update reacquires the OS lock; V1 non-Windows inheritance validates live PID/start identity; holder errors and migration/service platform gaps are explicit. V2 chooses fresh OS-lock+epoch acquisition everywhere and broad platform skips cannot satisfy parity. |
| PR #452, restore Windows consolidation coverage | Merged as `fc89e8be`, head `757fdb79` | Preserve FM-095's distinction between fail-closed unsupported production holder discovery and a scoped test-only offline guard. The complete platform-neutral consolidation/recovery suite executes on Windows; broad cfg exclusion cannot satisfy parity. |
| PR #451, release v0.0.58 | Merged as `81fe404c`, head `c5625c9e`, after #452 | Publication-only. Preserve merge order, source/tag/package/catalog/schema digests, and checks; the release receipt does not prove an installed runtime upgraded. |
| PR #409, superseded release attempt | Closed without merge | Historical inventory only. Do not require its version or deleted spec. |

### Post-baseline accepted-change refresh (`B`→`M`→`D`)

The table above pins the `81fe404c`/v0.0.58 baseline (`B`). The accepted base is
refreshed forward to implementation endpoint `M`
(`e560005610ac296018c3a16b9e6bded90de0eff5`, merge #462, v0.0.63) and audited
source/design endpoint `D` (`f18f0f14b3e7e2da30eefd9f1ed88862c0d73e57`). Evidence:
[`29-baseline-delta-audit.md`](29-baseline-delta-audit.md); operationalized
dispositions and fixtures: [`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md).
The `M`→`D` range is plan/architecture/governance only — intermediate drafts are
superseded by the surviving numbered files at `D` — so it carries no new V1
migration seam. The `B`→`M` runtime deltas below are the migration consequences.

| Change | State observed | Root migration consequence |
|---|---|---|
| PRs #454/#456/#458/#460/#462, releases v0.0.59–v0.0.63 | Merged `b1a3a13f`/`655296e4`/`2f3fac96`/`313d84c1`/`e5600056`; endpoint `M` = #462 | Publication-only accepted inputs. Refresh version/Cargo/manifest/catalog/schema digests and checks; no architectural dependency on release contents. |
| PR #453, runtime/CI hardening | Merged `8001a1f4` | Preserve Hermes projectless-compression routing (`STANDARD_HERMES_LCM_PROVIDER` `cursor`→`hermes`), registry/alias-aware `tracedecay_project_context` session-project resolution, Hermes-home prefix-containment rejection, cross-scope Turn correlation, and fixture normalization. Migration must add **provider continuity**: historical Hermes records labeled `cursor` are not remapped to `hermes` — backfill continuity or make the split explicitly queryable as two eras (FM-166). |
| PR #455, defer live-fact vacuum | Merged `41b2bdd4` | Preserve deferred exclusive-maintenance reclamation: `MemoryStore::remove_fact` no longer inline-vacuums; freed pages remain until exclusive maintenance. Migration/store must schedule a **periodic exclusive-maintenance cadence with reclamation receipts**, independent of upgrades (FM-164). Also carries replay identity during Hermes compression, compact hook routing, and bounded daemon teardown. |
| PRs #457/#459, managed-skill export isolation | Merged `a01ac4d9`/`227fad0b` | Preserve `AutomationRun`-only overwrite protection (`UserDraft`/`Import` foreign ownership authoritative) and default-profile export isolation. Migration must **canonicalize both sides** of the `uses_default_user_profile` predicate (symlinked `$HOME`/relocated `TRACEDECAY_DATA_DIR` must not silently no-op) and emit an explicit intentional-skip receipt (FM-161). |
| PR #461, safe upgrade shutdown messaging | Merged `ab983634` | Preserve quiesce/maintenance receipts in `src/update_cmd.rs`; the messaging adds no interrupt guard, so the lifecycle contract — not the print statements — owns bounded shutdown (FM-163). |
| Session sweep bump `user-turn-v1`→`user-turn-v2` (`src/sessions/hermes.rs`, via #453/#455) | In `B`→`M` | Migration must specify a **resweep budget** (CPU/IO/storage), prove interruption/resumption idempotency, and define orphan v1-cursor disposition (`skipped` reason `unavailable` or explicit cleanup); a new cursor namespace triggers a full user resweep (FM-165). |
| Per-destination projection fan-out and notifications (`plugin_init.py`, `session.rs`, analytics via #453) | In `B`→`M` | One turn projects sequentially to `[None, *project_roots]` (fail-open per root) and emits two event types, each `1 + unique_project_roots`. Migration/projectors add **per-shard idempotent receipts and catch-up reconciliation** (FM-162); PR 24FR/22H pin each event type's count, dedupe, and partial-failure behavior (FM-167). |
| Audited endpoint `D`, daemon-owned physical writers | `f18f0f14`; `architecture-boundaries.toml` + `tests/architecture_boundaries.rs` | Governance only: all five store entries are `physical_writer = "store"` plus semantic producers, machine-enforced. Preserve as the store-ownership boundary; no V1 migration seam. Post-`D` packet/remediation commits are review provenance. |

Every affected owner also carries the packet-30 §5 pointer. See
[plan 14 §7.6](14-historical-failure-regression-matrix.md) (FM-161–FM-171) for
the detection/recovery gates behind these consequences.

#418 and #425–#440 above are accepted-base behavior at the pinned publication commit where applicable. Any new open PR touching an owned V1 seam receives a live state/direct-file/check/semantic receipt before coding.

### Known baseline test behavior

`cargo test --workspace` currently allows the `session_suite` integration binary to share process-global state and can fail `structured_backfill::structured_backfill_one_shot_process_never_spawns` after another test sets `STRUCTURED_BACKFILL_LONG_LIVED_PROCESS`. The exact test passes alone and under nextest because it explicitly assumes per-test process isolation. `BACKGROUND_STRUCTURED_BACKFILL_ENABLED` and the long-lived marker are process-global atomics in `src/global_db.rs`.

This is a baseline runner-order defect, not a V2 parity waiver:

- `cargo nextest run --workspace --no-fail-fast` is the authoritative full-suite gate during coexistence.
- PR 3R changes the test to spawn a fresh `current_exe()` child probe for the one-shot/long-lived assertions, so plain libtest order cannot invalidate it.
- No V2 execution PR may claim a new failure is “the known baseline” unless the test name, output, and isolated rerun match this exact case.
- The final root gate runs both nextest and plain `cargo test --test session_suite`; both must pass before PR 35 begins.

## 5. Target Root Topology

### 5.1 Process boundaries

| Process | Opens | Owns | Must not do |
|---|---|---|---|
| `tracedecay` one-shot read | authenticated daemon client and manifest-only bootstrap status | request context and output adapter | Link/open SQLite, start an embedded application/store fallback, or claim database contents from bootstrap manifests. |
| `tracedecay` one-shot command | authenticated daemon workflow client | operation-specific read-only `inspect` or immutable `plan` followed by separately authorized `start`, idempotency key, receipt rendering | Hold/open DB transactions or bypass daemon authority. |
| Provider hook command | root `v2::hooks`, local spool client, pinned catalog/policy facts or bounded application hook port | normalize, durably queue, evaluate within budget, render one host response | Open graph/dashboard, scan repositories, perform inline backfill, or block on network. |
| MCP stdio server | root composition plus application/API-independent MCP adapter; mandatory same-version daemon client | exact protocol/catalog handshake, tool dispatch, markdown/JSON rendering, response handles | Own domain SQL, hand-maintained tool semantics, proxy to a stale daemon, or execute an embedded fallback. |
| Long-lived daemon | V2 store writers, capture drain, projectors, scheduler/automation workers, catalog publication, local/API sockets, source/effect-broker coordination | fenced leases, recovery, shutdown/checkpoint, background work | Accept a stale writer epoch, mix profiles across a connection, or read/mutate user files directly in strong mode. |
| Dashboard/API server | daemon-owned root `v2::api`, application kernel, static asset service, V1 route nest during coexistence | local/protected-remote auth, HTTP/SSE, SPA delivery | Open a second store authority or call legacy dashboard SQL/services after a route is V2-default. |
| Official external API/SDK client | service-owned local socket/pipe or authenticated loopback endpoint, current protocol/catalog handshake | bounded request/stream/operation lifecycle only | Open internal stores, use dashboard/MCP as a proxy, or fall back to a retired binding. |
| Enrolled remote Brain client | local sanitized capture spool, generated authenticated client, optional verified read cache | node handshake, idempotent upload, receipt retirement, cache watermark, offline state | Open authority SQLite/WAL, project canonical state locally, self-promote, or treat VPN identity as application authorization. |
| Remote Brain authority/standby | only its placed host-local stores or verified standby manifests | fenced authority epoch, enrollment/grants, semantic snapshot/tail, backup/recovery receipt | Mount database files remotely, accept stale epochs, or permit two writers. |
| Extraction worker | bounded code-source adapter and grammar registry | authenticated request, parse result/observation production | Resolve profile/routes or write canonical stores directly. |
| User-side source broker | registered provider/repository locators plus read-only source adapters and authenticated daemon capture client | discover/frame/normalize/sanitize typed observations and immutable code snapshots under the client identity | Receive a TraceDecay store path, import `StoreFactory`/canonical repositories, write SQLite, or scan outside registered grants. |
| User-side effect broker | authenticated daemon effect channel plus short-lived signed operation grant | execute only registered user-owned filesystem/Git/worktree/host-config/task-workspace effects with race-safe primitives and return typed receipts | Read a TraceDecay store, accept arbitrary paths/commands, widen a grant, perform canonical decisions, or replay an uncertain effect without reconciliation. |
| Service-manager isolation probe helper | fixed signed challenge/probe manifest and configured client test identity | launch the platform-native negative probe under the real client identity and return content-free connect/deny evidence | Read database bytes, accept caller paths, persist privilege, or replace runtime ACL drift verification. |

### 5.2 Final dependency direction

```text
tracedecay-domain
  ↑
store / capture / projectors / query / policy / tool-catalog
  ↑
tracedecay-application
  ↑                         ↑
root::v2::{hooks,presentation,api,remote_brain_transport}
          \             /
          root composition
       /      |       |      \
     CLI     MCP    daemon   host installers
```

The root may implement infrastructure ports declared by application/capture/store, but the port trait remains in the lower crate. Root-private `v2::remote_brain_transport` owns only HTTPS/mTLS listener/client, connection, stream, and semantic snapshot/tail wire adaptation; application/domain/store/query own enrollment, grants, placement, consistency, fencing, sync policy, and persistence. The root cannot expose V1 database rows, paths, global state, or transport types through those ports.

### 5.3 Root service graph

```rust
pub struct BootstrapContext {
    pub runtime_id: RuntimeIdentity,
    pub profile: ProfileSelection,
    pub principal: Principal,
    pub config: EffectiveConfigSnapshot,
    pub routes: RouteSnapshot,
    pub binary: BinaryBuildIdentity,
    pub brain: BrainBootstrapBinding,
}

pub struct BrainBootstrapBinding {
    pub role: BrainNodeRoleV1,
    pub brain_id: BrainId,
    pub node_id: BrainNodeId,
    pub node_epoch: NodeEpoch,
    pub authority_epoch: Option<AuthorityEpoch>,
    pub placement_version: Option<EntityVersionId>,
}

pub struct RootServices {
    pub application: std::sync::Arc<ApplicationKernel>,
    pub hooks: Option<std::sync::Arc<HookRuntime>>,
    pub api: Option<std::sync::Arc<ApiRuntime>>,
    pub remote_brain_transport: Option<std::sync::Arc<RemoteBrainTransportRuntime>>,
    pub operations: std::sync::Arc<OperationsAdapter>,
    pub compatibility: std::sync::Arc<CompatibilityRuntime>,
}
```

`BootstrapContext` is immutable per request/process connection. `RouteSnapshot` pins one config generation and catalog digest. Root builds lazy process-specific services; it does not create a 38-field `McpServer` or 20-field `DashboardState` replacement.

## 6. Exact Root File and Module Layout

### 6.1 Files created during coexistence

```text
src/
├── v2/
│   └── remote_brain_transport/   # HTTPS/mTLS and semantic sync wire adapters only; no authority or store semantics
│       ├── mod.rs                # private runtime facade and application-port binding
│       ├── listener.rs           # protected remote API/internal-node listener composition
│       ├── client.rs             # generated internal node-protocol client adapter
│       ├── handshake.rs          # framing only; semantic checks stay in application
│       └── streams.rs            # bounded observation/snapshot/tail/receipt framing
├── composition/
│   ├── mod.rs                    # exported RootServices factory only
│   ├── bootstrap.rs              # profile/runtime/config/build resolution
│   ├── process.rs                # one-shot/hook/MCP/daemon/API process profiles
│   ├── service_graph.rs          # lazy V2 and compatibility adapter wiring
│   ├── routes.rs                 # typed per-context read/write/effect routes
│   ├── flags.rs                  # persisted route config and emergency override parsing
│   ├── lifecycle.rs              # startup/recovery/shutdown ordering
│   ├── versions.rs               # binary/schema/catalog/policy compatibility
│   └── receipt.rs                # signed route/cutover/rollback receipt access
├── compatibility_inventory/
│   ├── mod.rs
│   ├── schema.rs
│   ├── snapshot.rs
│   ├── diff.rs
│   ├── disposition.rs
│   ├── render.rs
│   └── collect/
│       ├── mod.rs
│       ├── cli.rs
│       ├── mcp.rs
│       ├── http.rs
│       ├── dashboard.rs
│       ├── config.rs
│       ├── stores.rs
│       ├── providers.rs
│       ├── installers.rs
│       └── operations.rs
├── compat/
│   ├── mod.rs
│   ├── routing.rs
│   ├── parity.rs
│   ├── coverage.rs
│   ├── receipts.rs
│   ├── shadow/
│   │   ├── mod.rs
│   │   ├── normalize.rs
│   │   ├── compare.rs
│   │   └── report.rs
│   └── v1/
│       ├── mod.rs
│       ├── catalog.rs
│       ├── activity.rs
│       ├── graph.rs
│       ├── git_delivery.rs
│       ├── knowledge.rs
│       ├── policy.rs
│       ├── automation.rs
│       ├── accounting.rs
│       ├── operations.rs
│       └── payloads.rs
├── cli/v2_adapter/
│   ├── mod.rs
│   ├── request.rs
│   ├── output.rs
│   ├── errors.rs
│   └── domains/
├── mcp/v2_adapter/
│   ├── mod.rs
│   ├── dispatch.rs
│   ├── schemas.rs
│   ├── render.rs
│   ├── response_handles.rs
│   └── domains/
├── hooks/v2_compat.rs
├── mcp/hook_events_v2.rs
├── dashboard/v2_compat_api/
│   ├── mod.rs
│   ├── router.rs
│   ├── legacy_redirects.rs
│   └── parity.rs
├── daemon/
│   ├── runtime.rs
│   ├── protocol.rs
│   ├── workers.rs
│   ├── recovery.rs
│   ├── shutdown.rs
│   └── version_skew.rs
├── migrate/v2/
│   ├── mod.rs
│   ├── preflight.rs
│   ├── controller.rs
│   ├── checkpoints.rs
│   ├── reconcile.rs
│   ├── cutover.rs
│   ├── rollback.rs
│   └── archive.rs
└── bin/compatibility-inventory.rs

tests/
├── v2_root_composition/
├── v2_transport_parity/
├── v2_migration/
├── v2_release/
└── fixtures/v2/
    ├── v1-compatibility.json
    ├── root-route-matrix.json
    └── release-package-manifest.json
```

Files remain under their current paths until a target adapter is active; early PRs add adapters and route calls without mass renames. Production files target at most 400 lines; 800 lines is the hard default ceiling and requires a temporary plan-19 waiver. `src/composition/service_graph.rs` targets at most 500 lines and delegates each process profile to a separate builder.

### 6.2 Files changed first

- `Cargo.toml`: add workspace resolver/members, exact path+version V2 dependencies at their release phase, features, package includes, and explicit bins.
- `Cargo.lock`: generated dependency changes only.
- `src/lib.rs`: expose `composition`, `compat`, and `compatibility_inventory`; remove V1 public modules only in PR 37.
- `src/main.rs`: reduce startup to parse, build `BootstrapContext`, select process profile, dispatch typed adapter; preserve `ASYNC_STACK_BYTES`, extraction-worker authentication, help, exit codes, and startup maintenance until their owners move.
- `src/cli.rs`, `src/cli/**`, and root `*_cmd.rs`: preserve Clap shape while dispatching catalog-owned use cases one domain at a time.
- `src/serve.rs`: preserve project-discovery/template fallback and degraded behavior, then route through root composition.
- `src/daemon.rs` and `src/daemon/**`: preserve socket/service/watcher behavior while moving runtime pieces to the files above.
- `src/lifecycle_lease.rs`: retain PR #412's cross-process shared/exclusive/inherited-owner lease as the outer lifecycle fence; generalize its receipts and tests instead of replacing it.
- `src/mcp/server.rs`, `src/mcp/tools/**`, `src/mcp/transport.rs`: introduce generated catalog dispatch and thin V2 adapters without changing V1 names.
- `src/dashboard/mod.rs` and `src/dashboard/assets.rs`: nest the V2 API/router and new SPA while retaining legacy routes.
- `build.rs`: embed the new dashboard manifest/assets and generated catalog/OpenAPI digests; preserve package-from-crates.io behavior.
- `plugin/**`, `server.json`, `.claude/launch.json`, and host manifests: update generated hook/tool commands only after host conformance.
- `.github/workflows/**`, `release-plz.toml`, `.changeset/**`, `CHANGELOG.md`: coordinate workspace publication and compatibility notices.

## 7. Complete V1 Responsibility Ownership

Every current root module must appear in the PR 3R generated inventory. This table is the human audit anchor; the generated file is authoritative.

| Current files/responsibility | V2 owner | Root coexistence owner | Final disposition |
|---|---|---|---|
| `src/types.rs`, `errors.rs`, `serde_util.rs`, `timeutil.rs`, identity-like transport values | `tracedecay-domain`; application/API error envelopes | Root conversion/error adapters | Delete duplicate domain types; retain only root exit/error rendering. |
| `src/storage.rs`, `config.rs` path/layout pieces, `runtime_identity.rs`, `client_identity.rs` | `tracedecay-store` layout/catalog; application request context | `compat::v1::catalog`, bootstrap | Delete V1 layout resolver after #405 imports/cutover; retain root effective-config/profile selection only. |
| `src/global_db.rs` and tests | Store activity/catalog repositories, capture/projectors/query/application | `compat::v1::{catalog,activity,accounting}` | Delete table logic only when every table has import/parity/deletion receipt and no V1 route. |
| `src/db/**`, `branch.rs`, `branch/**`, `branch_meta.rs` | Store graph generations; code/Git projectors/query | `compat::v1::{graph,git_delivery}` | Delete branch DB write/read paths after graph/Git cutovers and packed-generation rollback closure. |
| `src/project_registry.rs`, `path_scope.rs`, `path_tree.rs`, `worktree.rs` | Domain ownership, catalog, application scope query | bootstrap plus V1 catalog adapter | Remove duplicate identity/scope calculations; retain only OS/Git discovery infrastructure behind ports. |
| Merged #425 `src/migrate/consolidate/**`, `src/open_store_holders.rs`, `src/sqlite_read_snapshot.rs`, consolidation manifest/CLI/doctor wiring | Store/application reconciliation workflow plus root migration controller | V1 split-store anti-corruption adapter under the shared lifecycle lease | Preserve historical behavior through operation-specific V2 consolidation inspect/plan/start/resume/status/verify/recovery and retain path-plus-file/inode holder semantics. Delete the V1 table-specific merger only after PR 33R/33S proves every family/disposition, remapped LCM edge, canonical path, dual backup, collision, and marker-cutover invariant. |
| `src/extraction/**`, `extraction_worker.rs`, `sync.rs`, `dependency_imports.rs`, `derive_table.rs`, `external_tools.rs` | Capture repository/code source adapter | Root code-source infrastructure port until PR 18 parity | Move scanner/worker/extractor implementation behind capture contracts; worker remains a root process entry, not a store writer. |
| `src/resolution/**`, `graph/**`, `redundancy.rs`, `ast_grep_search.rs` | Projectors for code evidence; query graph/code operators | V1 graph adapter | Delete V1 DB-bound resolution/query code after code-domain parity; preserve grammar-independent algorithms only in their owning crate. |
| `src/context/**`, `diagnose.rs`, `diagnostics/**` | Query/application code context and diagnostics observations/projections | V1 operations/query adapters | CLI/MCP names remain; move collection to infrastructure ports and orchestration to application. |
| `src/tracedecay.rs`, `src/tracedecay/**`, `sync.rs`, `monitor.rs` | Application use cases, store/capture/query, Observatory/accounting | Root legacy façade | Replace `TraceDecay` with `RootServices`/application calls; retire the all-in-one façade after every caller is cataloged. |
| `src/sessions/**` provider ingestion | Capture adapters and activity projectors | `compat::v1::activity` | Stop V1 source-head advancement per provider receipt; delete parser copies after V2 conformance and rollback closure. |
| `src/sessions/lcm/**` | Activity observations, `lcm_context_v1`, query/application | V1 activity/payload adapter | Preserve raw/source/summary DAG and payload lineage; delete duplicate canonical message store only after hashes/counts/coverage pass. |
| `src/sessions/git_correlation.rs`, workflow ingest/index/state | Git-delivery and agent-workflow projectors/policy | V1 Git/activity adapters | Preserve direct vs inferred evidence and native workflow fields; delete after backfill/correlation calibration. |
| `src/memory/**` and graph-resident fact tables | Store activity/project histories, knowledge projector/query/policy/application | `compat::v1::knowledge` | Migrate immutable fact/entity/trust/feedback/deletion lineage before any branch graph DB deletion. |
| `src/hooks/**`, `hook_cmd.rs`, `mcp/hook_events.rs` | root `v2::hooks`, capture, policy, application, tool catalog | V1 effect owner plus `hooks/v2_compat.rs` shadow | Delete per hook point at its accepted cutover after archived replay, current installer publication, host restart proof, and rollback drill. No stale descriptor remains callable. |
| `src/analytics.rs`, `analytics_bridge.rs`, `runtime_telemetry.rs`, `accounting/**`, `cost_cmd.rs`, `global.rs`, `cloud.rs` counter portions | Accounting/observability projectors/query/application | V1 accounting adapter and root optional network infrastructure | Preserve denominators/caps/upload consent; split version-check/update network calls from accounting before deletion. |
| `src/automation/**`, `automation_cli.rs`, `cli/automation.rs` | Projectors, policy, application commands/workflows | V1 automation adapter | Do not recreate #407-removed Hermes bridges. Import JSON/JSONL/files as immutable evidence; cut mutation owners independently. |
| `src/agents/**`, `agent_cmd.rs`, plugin materialization | Root host-installer infrastructure plus catalog/hook bindings and autonomous managed-skill materialization port | Root installer remains authoritative | Split `agents/mod.rs`; retain safe merge, backups, detection, install/update/uninstall, and host health. Managed skills are materialized by the autonomous curation worker under configured authority, not per-item commands. Remove hand-maintained tool/hook lists after catalog cutover. |
| `src/mcp/**`, `tool_command.rs`, `tool_command/**` | Application use cases, tool catalog, API-like schemas | Root MCP transport/render adapter | Preserve the 104-source-name baseline (102 at the older frozen inventory) and aliases; generated definitions replace hand lists; handlers disappear domain by domain. |
| `src/dashboard/**`, `dashboard/*` legacy plugins | API + frontend plan 11 | Legacy router/plugins nested beside V2 | Delete each plugin only after route, action, URL, accessibility, and data parity plus one release of redirects. |
| `src/cli.rs`, `src/cli/**`, root `*_cmd.rs`, `commands.rs`, `display.rs`, `status_cmd.rs` | Tool catalog + application | Root CLI adapter | Preserve help/flags/stdout/stderr/exit/JSON; remove command-local business logic after transport parity. |
| `src/daemon.rs`, `src/daemon/**`, `lifecycle_lease.rs`, `serve.rs`, `shell.rs` | Root process lifecycle; application/hooks/API ports | Root composition | Retain thin lifecycle/IPC and #412 cross-process fencing; remove DB/query/business logic and global background toggles. |
| `src/migrate/**`, `doctor.rs`, `doctor/**`, `retention.rs` | Store/application operations and root migration controller | V1 operations adapter | Map historical operations onto operation-specific inspect/plan/start/status/resume/verify/reconstruct/rollback/repair/cleanup commands; retire V1-specific implementation after archive window. |
| `src/upgrade.rs`, `upgrade/**`, `update_cmd.rs`, release workflows | Root release/upgrade | Root | Retain, add V2 preflight/backup/version-skew logic, and verify isolated crates.io install. |
| `src/git.rs`, `src/text.rs`, `src/resources/**`, `src/startup_tests.rs`, `src/user_config.rs` | Git infrastructure ports (capture/projectors/query), owning-crate text utilities, root embedded resources, root startup self-checks, plan 20 configuration registry (`UserConfig` is already a Section 10 config source) | Root infrastructure and V1 config/catalog adapters | Delete after owning-crate parity; `user_config.rs` legacy readers retire in PR 37G once generated config bindings pass parity. |
| `src/bench.rs`, benches/evals/tests | Owning crates plus cross-system root gates | Root compatibility harness | Move fixtures/benchmarks with behavior; never delete the only parity oracle with its implementation. |

`src/lib.rs` inventory CI fails if any public module/file is absent from this matrix or the generated machine-readable disposition.

## 8. Source and Store Ownership During Migration

| Source family | V1 authority before cutover | V2 canonical destination | Import/cutover invariant |
|---|---|---|---|
| Repository/profile identity | #405-aware `src/storage.rs`, `GlobalDb` registry/manifests, and merged #425 split-store ledger/backups/table dispositions | `catalog.db` identity allocations/shards/aliases/receipts | Import all candidates and receipts first; ambiguity blocks project cutover. Consume #425 plans/receipts as evidence: normalize paths, freeze both SQLite families, identify/reject holders by path plus file/inode, reserve writes, back up both inputs, preserve remapped LCM edges, verify every table/collision/count/hash, then publish marker/registry atomically. |
| Provider transcripts | `src/sessions/**` plus `sessions.db`/global activity rows | profile `activity.db` observations, actors, agents, sessions, turns, messages, tools, goals, workflows | One source head per artifact/generation; preserve native rows and unknown fields; no required project. |
| LCM raw/summary/payload | `src/sessions/lcm/**` and payload root | activity observations/projections plus privacy-domain blobs | Canonical message content stored once; DAG/source ranges and compression state retain exact lineage. |
| Hooks/hints/outcomes | hook process writes, analytics DB/JSONL fallback, V1 policy state | activity/project observations, policy/hint projections, catalog/policy digests | V1 is sole effect owner in shadow. Delivery and terminal outcomes never inferred from mere emission. V1 analytics/hook JSONL hint emissions map to plan 06 `HintOutcomeRecordV1` rows, making the historical 1,182-emitted/3-acted join queryable. |
| Code index/diagnostics/tests | branch `tracedecay.db` files | immutable graph generations and project evidence | Import snapshot identity, extractor/resolver version, diagnostics/test map, and privacy-domain-keyed file fingerprints; branch store is not deleted yet. Per-branch graph DB migration detail lives in `25-code-intelligence-indexing-crate.md`. |
| Git/delivery | branch/worktree metadata, Git correlation tables, daemon watcher, live remote reads | project events/relations and explicit local/live revisions | Keep direct, impacted, candidate-test, and context-only membership separate; drift blocks joined claims. |
| Memory/facts | `src/memory/**` tables inside `tracedecay.db` | activity or project histories by `DeclaredScope`; blob refs where needed | Facts/entities/trust/feedback/deletions are durable. Verify row/version/hash/link counts before graph DB archive. |
| Automation/skills/curation | JSON/JSONL/config/artifact roots and V1 tables | activity/project histories and privacy-domain blobs | Preserve historical proposal/approval/apply evidence, then model V2 candidate/validation/autonomy-decision/automatic-effect/use/outcome/revision/recovery chain. |
| Hermes legacy | #407 `src/migrate/hermes.rs`, `~/.hermes` sources/ledgers | ordinary user profile; activity/project chosen by declared scope | Never create a Hermes profile. Facts-only stores and collision receipts are mandatory. |
| Transitional user memory | shipped `~/.tracedecay/user-memory.db` from PR #441 | profile-scoped fact/entity/version/trust/feedback/relation histories in `activity.db` | Import once with stable source aliases, versions, trust, vectors, feedback, tombstones, and relation provenance; verify counts/hashes/links, then retire the standalone file after the normal read-only window. It is never a live V2 authority. Every Hermes named host profile, Codex, Claude, and Cursor shares this same user-global TraceDecay profile/store. |
| Hermes kanban boards / task graph | per-board `<root>/kanban/boards/*/kanban.db` plus default `<root>/kanban/kanban.db` (`tasks`, `task_links`, `task_runs`, `task_events`, `task_comments`, `task_attachments`, `kanban_notify_subs`, embedded dispatcher state) | plan 24 canonical task/plan/attempt records, dependency edges, versioned context packets, and privacy-domain attachment blobs | Import mapping is owned by plan 24 §16.2; this row is the cross-reference only. Each retained task receives a fresh canonical UUID `WorkItemId`; its native identity is a unique alias tuple `(source_manifest_id, board_slug, native_task_id)` whose safe rendering may be `hermes:<board>:t_<hex>`, resolving board-local collision without making that string canonical (FM-098). Task/link/run/event/comment rows are retained through those aliases; `blocked` is retained only after replaying `task_events` to classify sticky vs circuit-breaker, never from the status column (FM-097); in-flight claim state (`claim_lock`/`claim_expires`/`worker_pid`/`current_run_id`) is `skipped` (skip_reason `unavailable`) and re-claimed under a fresh mandatory fence epoch (FM-099); duplicate `idempotency_key` rows dedupe to the newest non-archived (older `skipped`, skip_reason `ambiguous`); unresolved `task_attachments` absolute paths and `[swarm:blackboard]` comment blobs are `quarantined`, and secret-bearing spans the PR 7E sanitizer flags are `redacted`; `kanban_notify_subs` cursors and single-host dispatcher state are `skipped` (skip_reason `unavailable`). Staged by the PR 33R controller (Section 14.1 phase 6) through plan 02's PR 33S/33S-2 executor over PR 7E capture-sanitized batches; task-graph ownership cuts over at PR 35J (Section 14.2). Requires the #407 ordinary-user-profile seam — no Hermes profile is created and `src/migrate/hermes.rs` stays a read-only source. |
| Analytics/accounting | analytics DB, hook logs, session usage, pricing config | accounting/observability events and projections | Unknown denominator remains unknown; caps/sample windows/source versions are first-class. V1 analytics migration rows live in `26-observability-accounting-and-usage.md`. |
| Dashboard/settings/provider manifests | project/user JSON/TOML/JSONC and host-owned config files | effective config read model plus config/installer audit events; external file remains authoritative where required | Preview/merge/backup/restore is idempotent. Never overwrite unrelated user config. |
| Response handles/artifacts/backups | response-handle root, LCM payload root, automation artifacts, V1 backup files | privacy-domain blobs, immutable export/backup manifests | Hash and permissions verify before publication; the compatibility response-handle binding remains for exactly one full release after its domain cutover (the same bound as the V1 read-only window), then fails typed with the durable anchor/export replacement. |
| Retention/GC bookkeeping | `src/retention.rs` state, GC ledgers/markers inside V1 stores | store operations events and retention projections | Import retention decisions/holds as immutable evidence; V2 GC never re-runs from stale V1 bookkeeping after cutover. |
| Runtime/daemon logs, crash, and telemetry files | daemon/hook log files, `runtime_telemetry.rs` on-disk output, crash reports | version-stamped accounting/observability events plus log-safe archives | Sanitizer-scanned before archive; never imported as canonical activity; secret canaries block publication. Use `KnownExactBuild` only when component+SemVer+build manifest are proven; use `KnownVersion` with source manifest when component+SemVer are proven without exact build; use `UnknownLegacy` with source manifest/reason for pre-contract or ambiguous rows. Never fabricate the importing/current build. Migration receipts count exact-build, known-version, unknown, redacted, quarantined, and skipped rows. |
| Lifecycle-lease and service-unit state | `lifecycle_lease.rs` on-disk lease files, systemd/launchd service units and state | root lifecycle receipts and service-state snapshots | Inventoried as on-disk artifacts; exact pre-operation service state restorable; #412 semantics preserved. |

This table is family-level by design. PR 3R additionally emits a file-level path/glob appendix for every family in the generated inventory, so store completeness is auditable against the tree before the migration controller exists. For the Hermes kanban family the appendix enumerates the per-board glob `<root>/kanban/boards/*/kanban.db` and the default `<root>/kanban/kanban.db`, each board DB's `tasks`, `task_links`, `task_runs`, `task_events`, `task_comments`, `task_attachments`, and `kanban_notify_subs` tables, and the `-wal`/`-shm` sidecars, so no board is silently omitted before the plan 24 mapping runs.

After a source-family capture cutover, V2 is the only canonical source-offset owner. During pre-cutover shadow only, the parity harness may derive V1-shaped comparison rows from V2 events. No post-cutover old transport is served. V1 may not continue independently parsing the same source; that would create split-brain even if IDs usually dedupe.

## 9. Route, Shadow, Cutover, and Rollback State Machine

```rust
pub enum BoundedContext {
    Capture,
    ActivityProjections,
    CodeGraph,
    GitDelivery,
    Knowledge,
    PolicyHintsHooks,
    AutomationSkillsAccounting,
    SessionTemporalRetrieval,
    TaskOrchestration,
    ProductReadsTransports,
}

pub enum RouteMode {
    V1Authoritative,
    V1WithV2Shadow,
    V2Authoritative,
}

pub struct DomainRoute {
    pub context: BoundedContext,
    pub mode: RouteMode,
    pub config_generation: u64,
    pub receipt_id: Option<ManifestId>,
    pub freeze_watermark: Option<VectorWatermark>,
}
```

Allowed forward transitions are linear. During the bounded PR 35 migration-validation window, a signed operator rollback may return the whole context to `V1Authoritative`; after PR 36 declares V2 default, V1 can no longer become a live owner. There is never a per-request or per-client fallback route:

```text
V1Authoritative
  -> V1WithV2Shadow
  -> V2Authoritative
```

Rules:

- `RouteSnapshot` is validated as a whole. Product reads cannot become V2-default before their required projections/query/application routes are V2-authoritative. All long-running processes pin its protocol/catalog generation and terminate/restart when it changes incompatibly.
- Shadow calls receive the same normalized request and captured time/vector watermark. A live drift marker is not a parity mismatch.
- Read shadow may execute both paths. Mutation/hook/automation shadow executes V1 only and computes a non-effecting V2 preview/evaluation.
- Each comparison class is `exact`, `expected_normalization`, `redacted`, `quarantined`, `v1_bug_compat`, `late_after_watermark`, `unavailable`, or `unexplained`. `unexplained` blocks transition. Comparison classes describe shadow parity results only; per-entity backfill accounting uses the Section 14.1 disposition vocabulary.
- A cutover receipt (`CutoverReceiptV1`, allocated in the `ManifestId` space; plan 19 §12.2 step 5 records this same schema) records binary/commit, V1 inventory and store manifests, V2 schema/registry/catalog/policy/projector digests, source/vector watermarks, counts/hashes, accepted differences, feature route, lease epochs, backup, tested rollback, the plan 18 PR 33A retroactive-privacy-audit remediation/restore-eligibility receipt for the context (zero synthetic canary hits), and the plan 14 `FM-###` rows whose gates the cutover satisfies.
- Route publication uses stage -> validate -> fsync -> atomic rename/CAS generation. Every process rejects a partial or digest-mismatched route set.
- At V2 cutover, V1 public routing and stale tool/protocol bindings are removed atomically. A stale client receives a typed `client_update_required`, `daemon_restart_required`, or `capability_replaced{current_binding}` error from plan 17's stale-client error-code registry, naming the current replacement; the server never executes a legacy alias or opens V1 on its behalf.
- Before PR 36 only, rollback is an explicit current-binary operator workflow: quiesce all processes under the lifecycle lease, fence V2 writers/effect owners, restore the V1 source position/owner recorded by the receipt, publish one lower route generation, restart current clients, and retain V2 evidence read-only. It is not an always-on fallback.
- After PR 36, rollback means a prior compatible V2 binary/schema or restore/reimport of non-disposable data into V2. The read-only V1 archive protects source data and evidence but can never be re-enabled as a live transport/store owner. Its expiry controls physical deletion, not protocol compatibility.

**Receipt signing.** “Signed” means HMAC-SHA-256 with a profile-local signing key stored in the profile catalog (`catalog.db` key table: `key_id` primary key, OS-protected key-material reference, `created_at`, `retired_at`), not asymmetric PKI. Every receipt, route snapshot, and rollback record carries its `key_id`; rotation is a typed operator command that mints a new key and retains retired keys for verification until every artifact referencing them is superseded or archived. Route load re-verifies the HMAC before use; verification failure is a typed fail-closed error that rejects the entire route set. Plan 19's signed inventories/receipts reuse this mechanism by reference.

**Rollback drill record.** Every drill and every real rollback writes a `RollbackDrillRecordV1`:

- `drill_id: ManifestId` — primary key;
- `context: BoundedContext` and `receipt_id: ManifestId` — the `CutoverReceiptV1` exercised;
- `is_drill: bool` — real rollbacks record `false`;
- `restored_watermark: VectorWatermark` and `epoch_fence_evidence` — pre/post fenced lease epochs proving no stale writer survived;
- `started_at`/`completed_at` timestamps and measured downtime;
- `data_difference: none | expected_normalization | unexplained` — `unexplained` blocks the gate;
- operator principal and signing `key_id`.

Uniqueness `(context, receipt_id, started_at)`; indexes on `(context, is_drill)` and `receipt_id`; one row per drill/rollback, stored in the profile catalog migration-receipt tables beside the receipt it exercises and retained until that receipt's archive window closes. A gate requiring a “tested rollback” is satisfied only by a matching `RollbackDrillRecordV1` with `data_difference != unexplained`.

## 10. Configuration, Environment, Profile, and Runtime Identity

PR 3R inventories every field/default/source from `TraceDecayConfig`, `SyncConfig`, `TelemetryConfig`, `UserConfig`, Clap, provider templates, and environment access. At minimum it must classify `TRACEDECAY_DATA_DIR`, `TRACEDECAY_DAEMON_SOCKET`, sync/watch overrides, diagnostics prewarm, memory injection, model prices, global DB enable/disable, provider summary-child settings, offline/update behavior, worker token, project root, and executable overrides.

Precedence remains explicit and rendered by Settings/doctor:

1. typed request/CLI flag where the command allows it;
2. documented environment override;
3. project config;
4. profile/user config;
5. compiled default.

Required changes:

- `EffectiveConfigSnapshot` stores value source, validation result, sensitivity, restart impact, and digest; secrets are capability-presence only.
- Add persisted per-context V2 route config under the active user profile. Defaults remain V1 until a signed receipt exists.
- Emergency environment overrides are limited to operational tuning and diagnostics — timeouts, log verbosity, socket/path selection, offline mode, and read-only diagnostic modes. They cannot select V1 or V2 routing. Rollback requires the typed operator command, lifecycle lease, signed receipt, quiescence, and process restart; environment drift cannot bypass migration gates.
- Profile selection happens before global/catalog/store opens. PR #407 means Hermes commands/sources use the same active profile.
- Plan 28's role, `BrainId`, node enrollment, authority endpoint, placement, consistency/cache, sync/privacy class, replica, and standby settings resolve before any store opens. Tailscale-specific values are optional endpoint metadata only; ordinary HTTPS/mTLS remains complete.
- Every client/daemon/plugin handshake exchanges exact root build/protocol version, route generation, schema major, catalog digest, tool-catalog generation, and profile identity. Any mismatch that changes executable bindings rejects the connection before request/store use and reports restart/update/current replacement; there is no older-reader or stale-plugin mode.
- Ordinary configuration patches are direct application commands with inline full-snapshot validation/impact, expected revision, idempotency, atomic private write, backup, audit event, and typed restart/operation requirement. They have no generic preview/apply ceremony. Separately cataloged destructive system effects use an operation-specific inspect or immutable plan followed by separately authorized start. Root adapters never directly mutate config from HTTP/MCP.
- Existing host-owned config file semantics—JSON, JSONC, TOML, shell hook, permissions, backup/restore, unrelated-key preservation—remain installer responsibilities and have fixture parity.

## 11. CLI, MCP, HTTP, Dashboard, and Hook Compatibility

### 11.1 CLI

- Keep each current Clap command/flag/env/default/conflict/validation structure while its context remains V1-authoritative or shadowing. At cutover, publish the current generated command set atomically; a retired alias/argument errors with the current replacement and never dispatches old behavior.
- `src/cli/v2_adapter/request.rs` maps parsed values to one catalog `UseCaseId`; it cannot select stores or build SQL.
- `output.rs` proves semantic/output parity before cutover and preserves the current V2 text/JSON contract afterward; it does not carry indefinitely versioned legacy renderers.
- Native `tracedecay tool <name>` remains distinct from native CLI commands and preserves `--args`, stdin arguments, `--json`, `--dry-run`, project routing, and response handles.
- Migrate by domain, not by top-level parser rewrite. A command route flips only after golden input/output/error snapshots and side-effect receipts match.

### 11.2 MCP

- Generated catalog definitions replace `src/mcp/tools/definitions.rs` groups. Existing names/arguments/rendering are parity fixtures before cutover; the cutover catalog contains only current bindings. Retired names and stale schema generations fail with `capability_replaced{current_binding}` or `client_update_required` from plan 17's stale-client error-code registry and never route to a fallback handler.
- `src/mcp/v2_adapter/dispatch.rs` performs catalog lookup and invokes application; domain adapters map typed results to renderer view models.
- `McpServer` shrinks to connection/request context, catalog snapshot, application handle, renderer registry, response-handle compatibility, and transport. It owns no domain repository/service fields.
- Project/template discovery degradation, initialization root routing, multi-MCP coordination, timing, and daemon transport tests remain mandatory. Protocol/catalog/version degradation is forbidden: mismatch terminates initialization.
- Missing/unavailable capability renders a stable typed gap; it does not silently route to a similarly named or retired tool.

### 11.3 HTTP/dashboard

- Mount root `v2::api` routes and SPA under the same loopback server; retain the legacy router at its old paths only while route rows are migration-only.
- `src/dashboard/v2_compat_api` maps approved V1 read models into the early workbench only. It cannot grow new product business rules.
- Human browser `?tab=`/plugin page URLs may redirect once to V2 saved investigation state, including project, selection, filters, and view where representable. Legacy plugin API clients do not receive response fallback; their protocol generation fails and requires plugin update/reload.
- Every current writable dashboard action maps to a typed application command before its legacy plugin redirects.
- Old plugin assets remain packaged only until that view's cutover gate passes. Cutover removes the old runtime asset/API binding in the same release and requires clients to reload the current shell; read-only V1 data remains available to V2 application queries, not through the old plugin.
- Generated assets and OpenAPI/client/catalog digests are checked by `build.rs`; an sdist/crates.io build cannot rely on the Git checkout or `node_modules`.

### 11.4 Hooks and daemon notifications

- PR 24FR introduces `src/hooks/v2_compat.rs` and `src/mcp/hook_events_v2.rs` after PR 24A establishes `HookApplicationPort`, matching plan 07's PR 24F ownership.
- Shadow mode allows one V1 host reply/effect and one non-effecting V2 normalized/evaluation record. It cannot double inject, deny, sync, ingest, or attribute an outcome.
- Claude compatibility inventories the six V1 aliases against the independent current 30-event × five-handler-type × source/version matrix. It imports user/project/local/managed/plugin/component ownership and disable state without executing foreign definitions, preserves unsupported events as explicit gaps, and never dual-registers a V1 alias plus its V2 binding. Cutover is per exact generated event: notification-only metadata first, tool/post-batch and lifecycle capture next, prompt/context delivery next, then separately proven blocking/rewrite/permission/continuation/worktree/elicitation effects. Retirement requires current stock CLI/remote fixtures, source/frontmatter lifecycle, managed policy, host dedupe, async/platform/output privacy, and old-plugin deactivation/reload proof.
- Codex compatibility covers exactly `SessionStart`, `SubagentStart`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `SubagentStop`, and `Stop`; it never invents a separate Codex tool-failure event. Cutover order is notification-only session, post-tool, user-prompt, subagent lifecycle, compaction, permission-request, Stop continuation, then explicit pre-tool blocking/rewrite.
- PR 24E7 discovery inventories additive system/cloud/MDM/`requirements.toml`, user JSON/TOML, trusted-project JSON/TOML, session, and plugin default/manifest Codex sources without mutating foreign definitions, managed policy, project trust, exact-hash trust/disable state, or the one-off bypass. The generated TraceDecay package emits only its default `hooks/hooks.json`; no installer writes inline hooks or auto-trusts them.
- Host installers switch one generated descriptor/hook point only after conformance, latency, host-native diagnostics, and rollback. A running plugin with the prior descriptor is rejected; repair installs the current descriptor and requires host reload/restart. Claude skill/agent frontmatter hooks are also proven inactive after component exit, and no V1/V2 duplicate remains configured or host-deduped invisibly.

## 12. Provider Installers, Plugin Manifests, Skills, and External Files

The root remains the infrastructure owner for `src/agents/**`, `plugin/**`, `server.json`, `.claude/launch.json`, provider config discovery, and executable-path materialization.

Plan 08's canonical `HostIntegrationManifestV1` and plan 27's generated host-bundle projection are the only release inputs. The pure catalog-owned compiler produces unsigned deterministic `HostBundlePayloadV1` and scan/conformance/rebuild inputs for each host/package. PR 36R independently rebuilds, scans, conformance-tests, attests, and signs each payload into `HostBundleManifestV1`; it alone publishes the release set. At deployment time plan 09 combines a verified signed release payload with the current local runtime probe into `ResolvedHostBundleV1`. The integration workflow accepts that resolved artifact only after verifying the canonical integration-manifest and catalog digests, every package/component content digest, source and adapter versions, current capability-probe snapshot, exhaustive difference ledger, stock-host-conformance receipt, license/SBOM, secret-scan receipt, signature, and supported-host matrix row; the root deployment adapter cannot bypass that application decision. A marketplace listing, mutable cache path, ambient host config, or locally copied skill cannot substitute for this chain.

PR 36R is the sole owner of the signed release envelope and attestation contracts:

```rust
pub struct HostBundleReleaseAttestationV1 {
    pub payload_digest: ManifestDigest,
    pub release_scan_receipt: EntityRef,
    pub independent_rebuild_receipt: EntityRef,
    pub stock_host_conformance_receipt: EntityRef,
    pub provenance_receipt: EntityRef,
    pub sbom: ManifestDigest,
    pub license_inventory: ManifestDigest,
    pub supported_host_matrix_row: EntityRef,
}

pub struct HostBundleManifestV1 {
    pub payload: HostBundlePayloadV1,
    pub payload_digest: ManifestDigest,
    pub release_attestation: HostBundleReleaseAttestationV1,
    pub release_attestation_digest: ManifestDigest,
    pub signature: SignatureRefV1,
}
```

The attestation is keyed by `payload_digest` and holds the scan, independent-rebuild, stock-host-conformance, and provenance receipts created after pure compilation. The signature covers canonical `(payload_digest, release_attestation_digest)` bytes. The payload contains neither digest, attestation, receipt, nor signature, so the integrity graph is acyclic and no compiler output can impersonate a released package.

The release set is component-atomic even when a native marketplace is not: core, context, work, and operator candidates are uploaded disabled/unlisted; their signatures, package dependency edges, component digests, and immutable locators are verified; then one signed release index is promoted. If any host/package fails, the prior index remains current and all candidates stay unavailable. Operator is never recommended/default. Publication, installation, and doctor report exact host/version/surface coverage and an explicit unsupported/fallback disposition rather than inferring parity from another surface.

Required installer contract:

- Plan 08's one generated `HostIntegrationManifestV1` declares capture/hook/install/executor facets: supported hook points, commands, tool bindings, required capabilities, install scope, config file format, backup policy, health check, and uninstall ownership. Plan 09 owns one descriptor-driven integration lifecycle with authorization, idempotency, operation state, compensation, and receipts; root's `v2::host_deploy` port implementation supplies only discovery/probe/config/filesystem/process effects, while provider files supply declarative paths/formats and irreducible host hooks.
- Discovery is read-only. The exact install/update/repair/uninstall application commands use path containment, private temp write, syntax validation, protected backup, atomic replace, post-read verification, and compensation on failure; there is no parallel root command/state machine or generic preview/apply/rollback vocabulary.
- JSONC/TOML writers retain comments/unknown keys where current behavior promises it; otherwise they make the smallest structural edit and preserve unrelated content byte-for-byte when feasible.
- Installation is idempotent and distinguishes “already current,” “updated,” “partially installed,” “user customized,” “permission denied,” and “host unavailable.”
- `tracedecay doctor --agent <host>` reports binary/version, descriptor and installed bundle/package/component digests, hook/tool manifest parity, signature/SBOM verification, selected MCP registration/profile, capability/difference/conformance snapshot digests, supported-host matrix cell, config source, daemon/profile/route identity, trust/reload state, any exact CLI-only downgrade, and a repair-plan anchor. Versions and digests identify evidence; they never become authorization.
- Managed skill create/update/materialize/archive/recover is driven only by the application curation worker under versioned autonomy configuration. Root materializes a validated, catalog-referenced owned artifact to declared host targets and records the exact checksum/receipt; it exposes no per-item approve/install/rollback command.
- PR #407-removed Hermes-specific bridge/config/inventory files are not reintroduced. Hermes source/plugin installation uses ordinary profile paths and actor/workflow evidence.
- Uninstall removes only entries/files owned by an installation receipt. It never deletes profile data, V1/V2 stores, user facts, sessions, artifacts, or backups.

`src/agents/mod.rs` is split during PR 24E7 into `registry.rs`, `context.rs`, `filesystem.rs`, `backup.rs`, `json.rs`, `jsonc.rs`, `toml.rs`, `discovery.rs`, `skills.rs`, `health.rs`, and `git_hook.rs`; provider-specific mapping stays in provider files. The old monolithic module is deleted only after every public function has one owner and existing agent suites pass.

The accepted-base Phase-0 reuse ledger starts with 15 `AgentIntegration` implementations and nine separate `install_mcp_server` bodies, including exact Cline/Roo install/uninstall duplicates; PR 22A regenerates the live count and records drift. PR 24E7 first routes them through the generated descriptor/deploy seam; PR 37K removes the copied installers, host manifests, and owned config fragments after supported-host conformance and the archive window. Every remaining provider-specific function records the host semantic that prevents declarative lowering. Provider count growth may add manifest rows and fixtures, not another installer engine.

## 13. Daemon, Workers, Watchers, and Shutdown

### Startup order

1. Parse process profile without opening a store.
2. Acquire PR #412's shared lifecycle lease for normal service/read operation, or exclusive/inherited lease for update, migration, rollback, repair, service refresh, archive, or destructive maintenance. The lease is cross-process and profile-root scoped.
3. Resolve executable/build identity, user profile, request principal, and effective config.
4. Select `Standalone | Authority | RemoteClient | ReadReplica | Standby`, verify node enrollment/credential and remote handshake where applicable, and bind `BrainId`, placement generation, authority/node epochs, grants, schema/catalog/privacy versions, and causal frontier before store open.
5. Load/verify route snapshot and exact client/protocol/catalog/policy/schema compatibility; stale processes fail before store opens.
6. Acquire only required V1/V2 store/writer leases; refuse ambiguous #405 identity and any placement/authority-epoch mismatch. A remote client opens only local spool/cache state.
7. Run V2 store/spool/staging/outbox/sync-receipt recovery before accepting writes.
8. Open V1 migration readers/writers only while the context is pre-cutover or an explicit rollback is active.
9. Start capture drain everywhere; start canonical projectors, scheduler/automation, curation, leases, retention, and effectors only for locally authoritative shards.
10. Bind socket/API, publish readiness with role/authority/placement/vector/cache/sync state, then accept clients.

### Runtime rules

- One daemon process owns live V2 shard writers through fenced epochs. Other processes submit to spool/IPC and never open a competing writer.
- The same daemon owns ordinary read pools, query execution, WAL checkpoints, integrity probes, and consistent snapshots. CLI/MCP/API/dashboard/SDK/provider processes receive no database path/handle and never open even read-only SQLite. Strong isolation readiness verifies the dedicated service identity, database-root ownership/ACL, IPC ACL, and key authority from plans 01/18; a same-user install is explicitly `SameUserDegraded`, not silently called protected.
- Install/update migrates strong-mode service identity, state-root ownership, socket/pipe ACL, key ownership, and service definition transactionally with backup/compensation receipts. Linux systemd, macOS LaunchDaemon/service account, and Windows service virtual-account/DACL fixtures prove the ordinary client identity can connect to the authorized endpoint but cannot traverse or read database families. An unprivileged portable install may select `SameUserDegraded` only with visible impact and a later migration path; it cannot claim or inherit strong status.
- The root package emits two binaries without adding a workspace package: user-facing thin integration binary `tracedecay` links generated client/presentation, manifest-only lifecycle bootstrap, hook/spool, read-only capture/source-broker, and typed user-effect-broker modules; private `tracedecayd` composes application, query, store, and daemon authority. Mode/capability lints keep MCP/ordinary CLI on daemon-client paths and keep source/effect execution behind explicit broker entry profiles. The maintenance entry is a private `tracedecayd` mode started by the service manager under the same dedicated identity. Link/source lints prove `tracedecay` cannot reach `tracedecay-store`, concrete V2 store constructors, layout/path helpers, canonical repositories, or SQLite store capabilities; provider-source SQLite reads are the explicit non-TraceDecay-store exception.
- The dedicated daemon identity receives no broad read access to a user's home directory, repositories, or provider transcript stores. User-side hooks and a bounded source-broker/extraction worker read only registered provider/repository inputs, sanitize and frame typed observations or immutable code snapshots, and submit them through the authenticated local protocol. Provider-source SQLite adapters may exist only inside that broker boundary; they cannot import TraceDecay store layout, `StoreFactory`, or canonical writers. An explicit per-project ACL grant is optional, auditable, and narrower than the portable broker default.
- User-owned mutations are a separate typed effect-broker boundary, never an expansion of the read-only source broker. Application use cases issue short-lived signed grants bound to Brain/profile, principal, operation/use-case ID, exact registered root/object and expected file/inode/base/version manifests, allowed primitive/effect budget, idempotency key, expiry, nonce, and revocation generation. The broker uses no-follow/handle-relative race-safe filesystem primitives for `move_symbol`, Git/worktree changes, owned host integration/config edits, and contained task-workspace operations; it returns before/after digests and typed receipts. Uncertain outcomes enter reconciliation and cannot replay blindly. No generic shell/path grant or daemon home access exists.
- Source/effect brokers are launched on demand by the ordinary user's service/session manager through closed `tracedecay` entry profiles and authenticate a reverse local channel to `tracedecayd`; they are not daemon children and inherit no service credential. If the user session/broker is unavailable, source freshness becomes explicit partial coverage and a requested user-owned effect remains pending/unavailable with a retry boundary; the daemon never falls back to impersonation, broad ACLs, or its own filesystem access.
- Install/upgrade and periodic strong-proof renewal use a narrowly privileged service-manager helper: the daemon supplies a signed nonce plus a fixed path-free probe-manifest ID; systemd/launchd/Windows Service Control Manager launches the packaged probe under the configured ordinary-client test identity, which proves endpoint connect and database-root/WAL/backup/key denial and returns content-free signed evidence. Between probes the daemon verifies owner/ACL/key/endpoint metadata drift. A missing helper, identity mismatch, failed challenge, or expired proof stops strong readiness; daemon self-inspection cannot renew real-identity evidence.
- Index/provider refresh is a typed daemon-owned operation with its own short-lived fenced epoch, operation ID, heartbeat/progress/checkpoint, exact shard/source/generation/overlay and target-watermark identity, canonical worktree where applicable, and terminal receipt; the daemon's process lifetime never doubles as an eternal sync lock. Concurrent CLI/MCP/API/dashboard requests join/queue/refuse against that operation through IPC and every joiner observes the same terminal coverage/error receipt. A stale lock is taken over only after process identity plus epoch death is proven, no command advises deleting a live owner's lock, and no clean/terminal state publishes before commit plus required checkpoint/durable close and manifest verification (FM-115/FM-153/FM-154).
- Doctor acquires a shared lifecycle lease before reading live stores; update/upgrade/migration/rollback/repair/service install-refresh-uninstall freshly acquire exclusive OS-lock ownership and CAS the lifecycle epoch before stopping the daemon, touching SQLite/WAL, changing binaries, or rewriting service files.
- The OS lifecycle lock, not owner text, sidecar, inherited token, PID, or process-start record, is mutating authority. Update holds it through binary replacement, releases immediately before launching the new binary, and every `post-update`/maintenance child must freshly acquire the exclusive lock and epoch before mutation; a handoff race fails closed. Merged #450's Windows fresh-reacquisition and non-Windows inherited-token validation are both fixtures, but V2 deliberately chooses fresh acquisition on every platform (FM-160).
- The exclusive lease owner first asks the daemon to drain, unlinks/stops admission, waits for client/background activity and writer release, then verifies WAL/lease quiescence. Timeout blocks the operation; it never proceeds against live writers.
- If the daemon dies before transferring the maintenance capability, the lifecycle coordinator admits recovery only after matching recorded process identity, service-manager stopped/dead state, failed authenticated endpoint handshake, and acquisition of the OS lifecycle lock. It advances an exclusive maintenance epoch before the service manager launches the private maintenance mode; all stale processes and leases are fenced. Client self-election and direct database recovery remain impossible.
- Worker supervision records start, ready, lag, retry, backoff, stopped, failed, and fenced state without prompt/path content in metric labels.
- Watchers emit source observations/effects; in dedicated-service mode they run inside the user-side source broker and submit through authenticated capture ports. They do not call graph/session/automation databases directly.
- Projector/scheduler/maintenance work has bounded queues, fair scheduling, cancellation, and disk/backpressure states exposed in doctor/Observatory.
- A hook remains useful when the daemon is absent: durable local spool if policy permits, explicit degraded acknowledgement, and later idempotent drain. No false “committed” receipt.
- A hook remains useful when the remote authority is unreachable: it sanitizes and durably spools locally, never blocks on network, and retires frames only after a verified canonical sync receipt. Cache/offline/pending state is never reported as committed authority state.
- Version skew is checked on every daemon handshake and route generation change. A stale daemon cannot continue writing after upgrade changes schema/catalog major.
- Service refresh snapshots and restores the exact pre-operation state: active/stopped plus enabled/disabled/persistently-masked/runtime-masked where the platform exposes it. A disabled or masked service is never silently enabled or started by update, doctor, or migration.
- Skill materialization ownership uses the single #411 predicate for doctor, autonomous update/archive/recovery, remove, and repair. A foreign-installation package remains untouched and informational; no autonomy decision or remediation advertises an effect the materialization path refuses.

### Shutdown order

1. Stop accepting new mutation/hook requests and publish draining state.
2. Atomically close spawn admission for schedulers, watchers, automation, scouts, migrations, sync/effects, provider subprocesses, and retries. Every admitted child is registered before launch under one shared supervisor; late and retry spawns remain rejected through terminal shutdown.
3. Drain bounded ingress to the durable acknowledgement boundary; do not wait indefinitely for projectors.
4. Cancel workers and terminate contained trees using Linux cgroup/service-scope or Windows Job ownership. macOS permits only probed sandboxed no-fork children; otherwise spawn is unavailable and observational scans never prove clean. Reap within one aggregate deadline, abort idle clients after bounded in-flight drain, and append stuck/retry/reaped receipts.
5. Checkpoint worker progress/outboxes and fsync spools/manifests while writer/lifecycle leases remain held.
6. Passive-checkpoint WALs within the existing 45-second daemon aggregate deadline; report the scalar busy flag plus log/checkpointed frames instead of forcing unsafe truncation, and never publish clean state without the mode-correct checkpoint-or-durable-close receipt.
7. Close writers, then release writer/lifecycle leases only after worker/client/descendant quiescence, checkpoint/durable-close, and receipt persistence are proven.
8. Close transports and service manager readiness.

Kill tests cover every startup/shutdown boundary, stale epoch, partial worker start, daemon upgrade, full disk, corrupt spool tail, locked reader, SIGTERM aggregate deadline, inherited lifecycle token, sync-owner/daemon death, PID reuse with process-start identity, drain timeout, live WAL, idle client, stuck child, concurrent retry spawn, Unix descendants, Windows Job descendants, and stopped/disabled/masked service restoration. The checked-in plan 14 lifecycle rows are the minimum case set; the kill-test receipt cites the exact `FM-###` rows it covers (at least FM-001–FM-010, FM-095, FM-096, FM-115, and FM-157).

## 14. Migration, Backfill, Reconciliation, Cutover, and Archive

### 14.1 PR 33R controller phases

`src/migrate/v2/controller.rs` is an orchestration state machine over store/capture/projector/application ports. The PR 33R/33S boundary is fixed: root PR 33R owns orchestration, phase sequencing, cutover/rollback receipts, and operator surfaces; plan 02's store-owned importer executor PR 33S owns storage-side import transactions, checkpoints, and parity counts; plan 02 PR 33S-2 separately owns store cutover support, rollback-window enforcement, and deletion proof; and plan 03's PR 7E capture path owns all V1 parsing plus mandatory sanitization — every V1 byte crosses the capture sanitizer and produces sanitization receipts before the store importer consumes sanitized batches (the capture-owns-V1-sanitize split):

1. **Inventory:** freeze V1 migration surfaces, store, provider, config, sidecar, payload, artifact, graph, session/LCM, memory, automation, #405/#407 identity/profile receipts, #411 ownership decisions, #412/#432 lifecycle state, and #425/#426/#428/#430/#434/#435/#436/#438 consolidation, registry, FTS-maintenance, graph-connection, applied-manifest retirement receipt, and authoritative registry evidence at a source cutoff.
2. **Preflight:** estimate V2 disk/WAL/blob/backup need with safety margin; normalize canonical platform paths; check permissions, symlinks, key availability, schema versions, unsupported/open holders, active writers, daemon version, and identity conflicts; acquire lifecycle/write reservations before computing the confirmation digest.
3. **Backup:** freeze each selected and legacy SQLite family without mutating either source; create and verify an independent backup of both families, including correct WAL state plus hashed sidecars/artifacts, and sign one manifest. Never copy a live DB file alone or let a successful backup of one input excuse a failed second backup.
4. **Identity:** import profile/repository/project/worktree/source aliases and persisted allocations before dependent entities.
5. **Activity:** import provider rows, native turns/messages, tools, reasoning markers, goals, subagents, workflows, LCM source/DAG/compression/payload lineage, hooks, and analytics.
6. **Project evidence:** import Git/delivery, code snapshots/graph generations, diagnostics/tests, project attributions, memory/facts/trust/feedback, automation/skills/curation, and accounting.
7. **Payloads:** classify/hash/verify and copy into privacy-domain blobs without cross-domain dedupe; quarantine missing/mismatched/secret-like inputs.
8. **Project:** run versioned projectors to immutable candidate generations; never mutate the prior validated generation.
9. **Reconcile:** compare counts, canonical hashes, ordinals, time, source offsets, aliases, graph nodes/edges, fact versions, trust/feedback, message-origin/representative views, LCM coverage and remapped source edges, artifacts, hint outcomes, table dispositions/collisions, and query fixtures at the frozen watermark.
10. **Receipt:** revalidate the deterministic confirmation under the same locks/reservations, publish signed import/parity/checkpoint manifests with every accepted difference and quarantine item, then and only then atomically cut the marker/registry route. A crash resumes the recorded ledger/staging state; doctor renders its exact status/resume/recover command.

Every phase is idempotent by source manifest/digest and has `not_started`, `running`, `complete`, `blocked`, `failed_retryable`, `failed_terminal`, or `rolled_back` state. Restart resumes the last committed checkpoint. Cancellation stops after the current atomic batch and leaves V1 authoritative.

Per-entity accounting uses exactly one disposition vocabulary across the plan set. Each phase manifest contains one `MigrationEntityDispositionRecordV1` per source entity:

- `manifest_id: ManifestId` — owning phase manifest;
- `source_digest: ContentDigest` — deterministic digest of the source entity;
- `source_family` — the Section 8 family;
- `entity_kind` — typed entity discriminant;
- `disposition: retained | skipped | quarantined | redacted | deleted` — the only per-entity disposition values anywhere in the plan set; the 00-index Phase 5 gate and plan 19 §§2.2–2.3 cite this schema rather than minting variants;
- `skip_reason: corrupt | ambiguous | unavailable` — required exactly when `disposition = skipped`;
- `target_ref` — canonical V2 ID/retrieval anchor, required for `retained`/`redacted`;
- `quarantine_ref`/`receipt_id` — required for `quarantined`/`deleted`.

Primary key `(manifest_id, source_digest)`; uniqueness one record per source entity per manifest; indexes `(manifest_id, disposition)` and `(source_family, disposition)`; one row per inventoried entity, bounded by the preflight estimate; stored in the profile catalog migration shard beside its manifest and retained until the run's receipt archive window closes.

Resumability is carried by `MigrationCheckpointRecordV1`: `checkpoint_id` (primary key), `run_id`, `phase`, `phase_state`, `batch_cursor` (opaque ordered cursor into the frozen source manifest), `source_manifest_digest`, `entities_processed: u64`, `committed_at`. Uniqueness `(run_id, phase, batch_cursor)`; index `(run_id, phase)`; same shard and retention as the disposition records. A batch commits atomically with its disposition records; restart resumes strictly after the last committed cursor, and a partially processed batch re-runs idempotently by `source_digest`.

### 14.2 Per-domain cutover order

PR 35 uses the master order and these dependencies:

1. PR 35A capture/source heads after durable spool and shadow conformance.
2. PR 35B sessions/agents/LCM reads after activity projections and #410 parity.
3. PR 35C code graph/diagnostics after immutable generation and graph/query parity.
4. PR 35D knowledge/facts after every graph-resident durable fact and lineage receipt passes.
5. PR 35E Git/delivery after local/live freshness and correlation calibration.
6. PR 35F policy/hints and hooks after exact replay, outcome attribution, latency, and host conformance.
7. PR 35G automation/skills/accounting after mutation/workflow/lease and outcome parity.
8. PR 35H product reads/transports after application/API/frontend/V1 action parity.

The route-context mapping is exhaustive and generated into the receipt schema:

| Slice | `BoundedContext` |
|---|---|
| 35A | `Capture` |
| 35B | `ActivityProjections` |
| 35C | `CodeGraph` |
| 35D | `Knowledge` |
| 35E | `GitDelivery` |
| 35F | `PolicyHintsHooks` |
| 35G | `AutomationSkillsAccounting` |
| 35H | `ProductReadsTransports` |
| 35I | `SessionTemporalRetrieval` |
| 35J | `TaskOrchestration` |

No slice shares a context or publishes a receipt under a broader surrogate; route snapshots, rollback drills, observation windows, and PR 37 retirement gates use the same exact variant.

Each slice runs V1 -> shadow -> V2-authoritative+compatibility, holds for the plan's observation window, drills rollback, and only then permits the dependent slice.

### Native semantic code-search cutover for slice 35C

The optional V2 native semantic lane has one runtime authority: root-private `src/v2/native_semantic_runtime`, using FastEmbed behind plan 04's consumer-owned port. It is a module in the existing root package, not another crate, service, database owner, query engine, or model downloader. The module accepts only plan-25 eligible document/chunk requests with the complete immutable model/revision/artifact/tokenizer/runtime-ABI/dimension/metric/normalization/formatter/chunk/privacy/key/source-generation pins and returns bounded vectors plus runtime receipts; it cannot open stores or publish generations.

PR 35C shadows lexical/graph results and FastEmbed semantic generations separately, reports semantic coverage explicitly, and cuts semantic reads over only after staged-build determinism, last-good rollback, reader-drain, incompatible-pin rebuild, no-mixed-vector, and performance receipts pass. V1 vectors and the February 2026 direct-`ort`/Nomic/brute-force design are never imported. Source documents are re-formed through plan 25 and rebuilt. Retirement removes direct runtime/model wiring and any parallel vector scan/query/storage path; rollback points to the retained last-good generation, never to the superseded implementation.

Master Phase 5 additionally defines PR 35I (plan 23 session/LCM/temporal retrieval cutover) and PR 35J (single scheduler/lease owner cutover, plan 24). PR 35I begins only after 35B and 35F complete their observation windows; PR 35J only after 35F and 35G; PR 35H publishes `V2Authoritative` for product reads/transports only after 35I and 35J complete theirs. Root owns route publication for all ten slices. The Section 8 Hermes kanban board / task-graph import is staged by the PR 33R controller (Section 14.1 phase 6) and its task-graph ownership cuts over inside PR 35J; the board slug/dir layout and single-host dispatcher are dropped, not ported (plan 24 §16.2; FM-097, FM-098, FM-099, FM-102).

### 14.3 Archive and physical deletion

- PR 36 makes V2 routes default but performs no V1 deletion.
- V1 stores become read-only, carry an archive manifest and last authoritative watermark, and remain inspectable only through current-binary migration/archive tooling for one full release. The one-release archive window opens at the PR 36 V2-default release for every store family; a per-context cutover does not start an earlier private window. They are not query backends for live product transports.
- PR 37 first exports/verifies the complete archive, imports it into a clean temporary V2 profile, and replays the representative parity corpus. It does not boot a stale V1 service.
- Physical deletion is a separate typed retirement `plan` followed by a separately authorized `start`, requiring explicit user confirmation, expected archive digest, no active hold, no active rollback receipt, and no retained replay reference.
- Deletion writes a durable receipt before unlink, then removes only manifest-owned paths. Unknown files are reported and preserved.
- A failed delete is resumable and never marks the archive absent until post-delete verification succeeds.

## 15. Giant Module and V1 Store Retirement Map

| Retirement slice | Delete/move | Preconditions |
|---|---|---|
| PR 37A, transport registries | Hand-maintained MCP tool definitions/dispatch policy duplicates, CLI use-case maps, dashboard plugin registry duplicates | Current generated catalog is the cutover generation; all clients/plugins restarted or rejected by handshake; no unowned schema/effect; archived catalog snapshots load for replay only. |
| PR 37B, global/activity stores | `src/global_db.rs`, `src/global_db/**`, V1 session/activity SQL and parse-offset logic | Catalog/activity imports, source heads, #410 views, analytics/workflow/Git rows, backup/restore, and all V1 read routes parity-proven. |
| PR 37C, branch graph/storage | `src/db/**`, DB-bound `src/graph/**`, `src/branch.rs`, `src/branch/**`, `src/branch_meta.rs`, V1 layout portions of `src/storage.rs` | Code graph generations/query parity; #405 identity receipts; every memory/fact/entity/trust/feedback row migrated; archive restore proven. |
| PR 37D, sessions/LCM/memory/automation | V1 implementations under `src/sessions/**`, `src/memory/**`, `src/automation/**`, `src/retention.rs`, replaced migration code | Native provider/LCM/knowledge/automation evidence parity, zero live V1 adapters after cutover, one-release read-only store archive/evidence window, no executable replay dependency. |
| PR 37E, hooks and legacy dashboard | V1 hook implementations, legacy dashboard API modules, `dashboard/{holographic,lcm,graph,savings,code-diagnostics,settings,hermes-wrapper}` | Per-hook installer/host conformance and rollback closure; every view/action/URL/accessibility/export parity row; redirects retained separately. |
| PR 37F, façade and dependency cleanup | `TraceDecay` all-in-one façade, unused root command/service helpers, direct `libsql` root dependency, obsolete build includes/vendor patch if no longer referenced | All call sites use application/composition; package/install tests pass; V2 driver/link ADR confirms whether `libsql-rusqlite` still requires any vendor component. |
| PR 37G, legacy configuration plane | V1 config file/env/flag readers, dashboard settings mutation paths, provider/hook/daemon setting duplicates replaced by plan 20 bindings | Owner: plan 20. Generated config registry parity for every Section 10 inventory row; every effective value served by the resolver with source/precedence; config audit receipts; rollback closure. |
| PR 37H, legacy scout/hint paths | Ad hoc suggestion/second-hint engines and duplicate delivery state replaced by plan 22 envelopes | Owner: plan 22. Delivery-arbiter and outcome-attribution parity, hook-latency budgets held, host conformance, rollback closure. |
| PR 37I, legacy session/LCM/search paths | Independent legacy FTS/LCM/ranking/load-routing code replaced by plan 23 retrieval | Owner: plan 23. PR 35I cutover receipts, plan 15/23 evaluation gates, anchor-hydration and export parity. |
| PR 37J, legacy board/scheduler/output paths | Hermes board/current-file dispatch remnants, duplicate scheduler/lease owners, legacy task output paths replaced by plan 24 | Owner: plan 24. PR 35J single scheduler/lease receipts, board-projection parity, fenced-epoch closure. |
| PR 37K, copied host installers and config fragments | Handwritten per-host skill/rule/command/agent/hook/MCP manifests, duplicated installer/config-writer bodies and permission/tool lists, obsolete plugin scaffolds, and repository-owned cached/generated fragments replaced by plan 27's compiler plus the root deploy adapter | Signed current host release set; canonical/resolved manifest and package/component digest parity; stock-host IDE/CLI/cloud conformance for every supported matrix cell; install/update/repair/uninstall and crash-compensation receipts; one full release of compatibility evidence. Delete only repository-owned sources and receipt-owned host entries. Foreign marketplace/plugin caches, user/team/workspace config, unknown fields, backups, unmanaged packages, and unproven files are reported and preserved byte-for-byte. |

Before deletion, run `rg` and TraceDecay impact/call tools over each symbol/file; copy test fixtures to the owning crate; produce a deletion receipt listing removed public exports, callers, replacement use case, archive dependency, and release window. No deletion PR combines more than one row above.

Final root source should contain only:

- binary/CLI parsing and rendering adapters;
- composition/bootstrap/process lifecycle;
- daemon IPC/supervision/watch infrastructure;
- MCP transport/render adapter;
- API/static asset embedding glue;
- hook executable compatibility/render entry points;
- provider host installers/materializers;
- upgrade/release and OS integration;
- infrastructure port implementations that cannot live in a platform-neutral crate.

## 16. Workspace Packaging, Versioning, Upgrade, and Release

The root is published to crates.io today, so path-only unpublished dependencies are not viable once V2 is linked. Use this release model:

1. New V2 crates begin `publish = false` while contracts are experimental and no released root package depends on them.
2. Before the first released root binary links V2, give every V2 crate the same workspace version as root and publish it as an implementation crate in topological order. Root dependencies use both `path` and exact `=x.y.z` version.
3. Generate and freeze the package DAG from plan 19's `architecture-boundaries.toml`, `cargo metadata`, and the generated-public-contract edge. Enforce the <=11-package ceiling. Publish `tracedecay-domain`; then the domain-only implementation wave (`tracedecay-store`, `tracedecay-capture`, `tracedecay-projectors`, `tracedecay-code-index`, `tracedecay-query`, `tracedecay-policy`, and `tracedecay-tool-catalog`); then `tracedecay-application`; then the official Rust `tracedecay-client` built from the same frozen public-contract digest and root with its private hook/presentation/API/remote-Brain-transport modules. Peers may publish concurrently only when the generated DAG proves no dependency edge. The Rust client has no Cargo dependency on domain, application, API implementation, or root; no crates.io artifact exists for a root-only adapter.
4. The release job waits until every artifact in a wave is registry-readable and matches its expected checksum before starting a dependent wave. It package-tests each crate from the registry artifact, not a workspace path, and rejects any manifest edge absent from the generated DAG.
5. `release-plz.toml` and workflows generate one coordinated release receipt containing the package DAG, package versions/checksums, frozen public-contract digest, lockfile, source commit, Rust toolchain, dashboard asset manifest, OpenAPI/client/catalog/policy digests, migration compatibility range, and the complete host release set: canonical integration-manifest digest, every unsigned host-bundle payload, signed release manifest/attestation, runtime-resolved bundle, package/component digest, capability-probe and difference report, stock-host-conformance receipt, signature/checksum, license/SBOM, supported-host matrix row, and secret-scan receipt.
6. Publish all native-host packages from those already signed artifacts as one component-atomic release index. Verify every candidate locator/digest/signature/dependency before promotion; a marketplace without transactions uses disabled/unlisted candidates and one signed index flip, never a partially current set.
7. `cargo package` and isolated `cargo install tracedecay --version <version> --locked` run without workspace paths, Git submodules unavailable at runtime, `node_modules`, or network asset generation.
8. A trusted-base release integrity job rejects changed files outside the generated release allowlist unless maintainers explicitly approve the exact extras; tracked ignored files, omitted generated contracts/specs, and a dirty release-plz tree block publication.

Upgrade flow:

- Inspect current binary/daemon/schema/catalog/policy/routes and disk/backup requirements before replacement.
- Stop or drain the daemon; create and verify a pre-migration backup before any forward schema migration.
- Install dependency crates/root artifact, verify signature/checksum, run read-only doctor/package/plugin checks, then run explicit forward migrations.
- Restart daemon and verify handshake version, route generation, store integrity, source/projector lag, host integrations, and dashboard assets.
- Binary rollback is allowed only when the old binary declares the current schemas/catalog compatible. Otherwise restore the verified backup and route receipt; never run a down-migration implicitly.
- Upgrade failure preserves the old executable backup and stores, prints an exact recovery command, and does not clean migration sources.

Compatibility/version rules:

- V2 schema/catalog/policy major mismatch: writes denied, metadata/read-only status allowed.
- New compatible minor: old reader exposes partial/incompatible coverage for unknown fields; it never drops them.
- Daemon/client root versions may differ only inside an explicit handshake range; mutation requires matching route/schema major.
- Provider manifest versions are independent but record required root/catalog/hook ranges.
- The V2-default release notes list route defaults, removed tool/protocol/plugin generations, current replacements, required process/host restart, V1 data archive location, operator rollback procedure, and future physical-removal version.

## 17. PR and TDD Execution Sequence

Commands run from repository root with the checkout-local `target/`. Do not set `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` unless Cargo reports actual target-lock contention.

### PR 3R: Compatibility inventory and baseline ledger

**Files:** create `src/compatibility_inventory/**`, `src/bin/compatibility-inventory.rs`, `tests/fixtures/v2/v1-compatibility.json`, `tests/v2_root_composition/inventory.rs`; modify `src/lib.rs`, `tests/session_suite/structured_backfill.rs`, `tests/session_suite/main.rs`.

- [ ] Write failing tests that every lib module, CLI command/flag, MCP tool/schema, HTTP route/action, dashboard panel/action, provider hook, config/env/default, store/table/index/trigger/sidecar, installer mutation, doctor/repair/migrate command, and release asset has one disposition/owner.
- [ ] Inventory every `cfg`/target gate, ignored/skipped platform test, test-only substitution, OS service/lock/holder/path primitive, and migration/backup/consolidation operation. Generate one disposition per `(operation, platform)`—`supported | alternative | unavailable | untested`—with production owner, exact test denominator, substitute boundary, recovery, and cutover gate. CI fails on broad module/suite exclusion, `untested`, a supported row with no executed lane, or a test substitute reachable in production (FM-095; merged #450/#452 fixtures).
- [ ] Add a fresh-child-process probe for `structured_backfill_one_shot_process_never_spawns`; prove plain libtest and nextest no longer disagree.
- [ ] Generate deterministic JSON plus human report with binary/commit/time/watermark/digests and statuses `v1_only`, `v2_shadow`, `parity_proven`, `v2_default`, `migration_only`, `retired` — the route-status axis, distinct from the Section 14.1 per-entity disposition vocabulary.
- [ ] Emit the file-level path/glob appendix for every Section 8 source family.
- [ ] Import #425's explicit consolidation table/disposition registry and prove every incoming source table, index, trigger, WAL/SHM sidecar, remapped LCM source edge, collision class, canonical platform path, holder check, reservation, backup, ledger state, marker write, and doctor recovery action has one V2 owner/test/deletion gate.
- [ ] Establish PR 3R as the single inventory generator: plan 19 §2.3's `target/tracedecay-v2-inventory/*` artifacts are generated views of this same inventory run (one generator, one vocabulary per axis); no second generator may be created.
- [ ] Run `cargo test --test session_suite structured_backfill_one_shot_process_never_spawns -- --exact`; expected: pass independent of suite order.
- [ ] Run inventory twice and compare hashes; expected: byte-identical after excluding explicit snapshot metadata.
- [ ] Commit `test(compat): freeze root v1 surface inventory`.

### PR 4AR: V1-backed read-only workbench composition

**Files:** create `src/compat/{mod,coverage}.rs`, `src/dashboard/v2_compat_api/**`; modify `src/dashboard/mod.rs`, `src/dashboard/assets.rs`, `build.rs`; use plan 11 prototype files.

- [ ] Add failing adapter tests for All/project scope, one session/turn/tool/subagent investigation, graph summary, coverage, direct reload, legacy URL, and zero writes.
- [ ] Implement bounded V1 read adapters only; label every response with V1 source/commit/store/freshness/cap.
- [ ] Prove the prototype cannot reach V1 mutation methods and that old dashboard routes remain byte/semantic compatible.
- [ ] Commit `feat(compat): serve v2 workbench from bounded v1 reads`.

### PR 7FR: Source-family shadow capture composition

**Files:** create `src/compat/v1/{activity,payloads}.rs`, `src/compat/shadow/**`, root capture port implementations; modify `src/sessions/**` entry composition only.

- [ ] Add failing tests that one source record yields one V1 authoritative effect and one V2 non-effecting observation, with fixed source/cursor/hash and no duplicate on retry.
- [ ] Enable shadow per provider/source family behind persisted routes; preserve V1 offsets and output.
- [ ] Record parity/quarantine/latency receipts; no route advances automatically.
- [ ] Commit `refactor(compat): shadow provider capture into v2`.

### PR 12AR: First end-to-end V2 composition slice

**Files:** create `src/composition/{mod,bootstrap,process,service_graph,routes,flags,lifecycle,versions,receipt}.rs`, remaining base `src/compat/**`; modify `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `src/serve.rs`.

- [ ] Write failing architecture tests for dependency direction, process-specific opens, route validation, one effect owner, no V1 type in application contracts, and lazy store access.
- [ ] Wire Codex + one project + sessions/tools/subagents from capture through store/projector/query/application/minimal HTTP/workbench.
- [ ] Demonstrate V1 read, V2 shadow, fixed-watermark comparison, partial coverage, saved investigation, export manifest, route rollback, and no V1 behavior change.
- [ ] Commit `feat(root): compose first v2 vertical slice`.

### PR 24E0: Strong service identity, split binaries, and local transport

**Files:** create `src/bin/{tracedecay,tracedecayd}.rs`, `src/v2/service_identity/{mod,linux,macos,windows,migrate,verify,probe}.rs`, `src/v2/api/local_transport/{mod,uds,named_pipe}.rs`, `src/v2/capture_ingress/{mod,protocol,service,client,acl,lifecycle}.rs`, `src/v2/source_broker/{mod,runner,grants}.rs`, `src/v2/effect_broker/{mod,grant,filesystem,git,host_config,task_workspace,reconcile}.rs`, `src/v2/subprocess_supervisor/{mod,admission,registry,shutdown,linux,macos,windows}.rs`, `src/v2/lifecycle_bootstrap/{mod,frame,journal,recover,rotate}.rs`, service/socket definitions/templates and cross-identity fixtures; move root daemon composition from `src/main.rs` behind the private daemon entry. Capture ingress is a closed socket-activated `tracedecayd` mode, not a third binary or second spool implementation.

- [ ] Install and migrate Linux systemd, macOS LaunchDaemon/service-account, and Windows service virtual-account identities; transfer state-root/key ownership transactionally with compensation receipts and preserve service enabled/masked/stopped state.
- [ ] Make the local UDS/named pipe service-owned with connect-only client ACL, peer identity plus token authentication, service-manager lifecycle bootstrap, and no client-visible store locator. Test real daemon and ordinary-client OS identities, including negative database traversal/read/open probes executed as the client identity.
- [ ] Add the socket-activated capture-ingress protocol and service-manager units: accept only bounded sanitized/authenticated plan-03 frames, fsync capture's one service-owned spool while the main authority is absent/draining, share lifecycle fencing and key rotation, and hand off to the normal drainer after restart. Install/update/repair/uninstall preserves prior service state and compensates both endpoint and spool ownership atomically. Linux/macOS/Windows tests prove the ordinary hook identity can connect but cannot list/open spool/store/key roots; kill, disk-full, stale-client, ACL drift, drain, and service-disabled cases never claim durability or create a client spool.
- [ ] Implement the user-side read-only source broker so strong mode can capture registered provider SQLite/transcript sources and repository snapshots without granting the daemon broad user-home access. Separately implement the signed, revocable user-effect broker for race-safe `move_symbol`, Git/worktree, owned host config, and contained task-workspace effects with expected-state, idempotency, receipts, uncertain-effect reconciliation, and per-operation conformance. Prove neither broker can import TraceDecay store/layout/canonical-repository capabilities.
- [ ] Implement service-manager-launched real-client isolation probes and periodic challenge/renewal on Linux/macOS/Windows; reject caller paths and database content, preserve content-free receipts, and fail strong readiness on expiry/identity/ACL drift.
- [ ] Implement fresh-OS-lock maintenance handoff and dead-daemon four-part death proof plus exclusive epoch CAS. Treat every inherited-token path as a V1 red fixture; kill-test every boundary and prove stale workers never open or publish.
- [ ] Implement `SubprocessShutdownReceiptV1` and its plan-02 projection through the one root-private supervisor. Failing FM-157 tests cover pre-spawn registration, spawn-fence races, retry denial, clients, stuck children, aggregate deadline, Linux cgroup, Windows Job, macOS sandbox no-fork probe/denial, fork/`setsid`/double-fork attempts, and zero-survivor-or-no-clean-publication. Architecture lint permits direct spawn only in this module/test harness.
- [ ] Implement the one service-owned lifecycle bootstrap journal by reusing plan 03's shared framed-segment codec/fsync/torn-tail/rotation kernel with a closed lifecycle frame registry and separate service-only root/key. Frames carry sequence+prior digest, HMAC key epoch, CRC, and catalog projection watermark. Checkpoint/close/subprocess receipts append here before gates advance, including when `catalog.db` is the target; no SQLite target self-certifies and no second journal engine appears. Tests kill before/after frame write/fsync/target checkpoint/target close/catalog projection/segment retirement, reject replay/tamper/gap/key mismatch, and prove clients cannot list/open/copy the journal.
- [ ] Add binary-link and source-architecture tests proving `tracedecay` contains only daemon client/presentation/lifecycle plus explicitly profiled hook/spool/source/effect brokers and never TraceDecay store/application authority; MCP and ordinary CLI modes cannot invoke broker entry profiles. `tracedecayd` alone composes application/query/store authority. The only pre-daemon ordinary-client effects are service install/start/status/recovery over manifest/service-manager metadata; offline hook/source frames remain capture-owned.
- [ ] Gate strong readiness on fresh variant-specific `StoreIsolationStatusV1` proof receipts; portable fallback remains explicitly `SameUserDegraded`.
- [ ] Commit `feat(root): isolate daemon storage authority`.

### PR 24E1–24E8: Thin root adapters

Dependency gates are explicit: plan 25 PR 18B–18F lands `tracedecay-code-index` and PR 18G proves its root/projector/store adapter before the 24E3 code binding can cut over; plan 21's root `v2::presentation` module lands before the first human renderer moves in 24E1 and every later CLI/MCP adapter imports it instead of adding local formatting; plan-10/17 contract generation freezes the digest used by root `v2::api` and the official `tracedecay-client` before SDK parity is claimed. Root adapters never depend on the official client package or call their own HTTP API in-process.

Each PR starts with catalog parity and failing cross-transport fixtures, then moves one bounded family:

1. **24E1 capability/profile/project/health:** create CLI/MCP adapter bases and composition bootstrap.
2. **24E2 sessions/messages/LCM/agents/workflows:** preserve #410 filters, sanitized-native expansion, ordering, response handles, and export.
3. **24E3 code/graph/diagnostics/tests/edits:** preserve project/branch/snapshot/fallback and bounded impact/path semantics plus #414 historical `move_symbol` execution/rollback/reindex evidence, mapped to operation-specific V2 inspect/commit/recovery contracts.
4. **24E4 Git/delivery/context:** preserve semantic Git tools, direct/impact/test/context membership, live/local freshness, and no remote mutation.
5. **24E5 knowledge/memory/policy/labs:** preserve fact/trust/feedback/curation and read-only replay.
6. **24E6 automation/skills/accounting:** preserve scheduler/lease semantics and every historical mutation/audit, then expose V2 autonomous curation decision/effect/outcome/recovery parity with no per-item preview/apply binding.
7. **24E7 provider installers/config/doctor/repair:** split `agents/mod.rs`, map operations to application jobs/commands, preserve safe host config edits.
8. **24E8 migration/backup/retention/export/response handles:** expose typed long jobs and current bindings; retired operation/tool names return the generated replacement error without execution.

For each:

- [ ] Run the exact V1 CLI/MCP/dashboard fixtures before change; save output/effect digest.
- [ ] Write failing V2 adapter and cross-transport parity tests.
- [ ] Implement conversion only; no domain logic.
- [ ] Run V1 and V2 fixture matrices; accept only named versioned differences.
- [ ] Flip that transport binding behind route config and drill rollback.
- [ ] Commit one adapter family; do not mix unrelated module cleanup.

### PR 24FR: Hook and daemon notification compatibility

**Files:** create `src/hooks/v2_compat.rs`, `src/mcp/hook_events_v2.rs`, daemon capture worker wiring; modify provider hook command dispatch/manifests only after fixture gates.

- [ ] Start only after PR 24A establishes `HookApplicationPort`; use plan 07's six PR 24F commit slices.
- [ ] Prove one reply/effect owner, durable acknowledgement truth, concurrent producers, fallback spool, policy/catalog digest, host rendering, and outcome evidence.
- [ ] Shadow then cut one hook point at a time in the plan 07 order; run native host diagnostics after each.
- [ ] Keep the prior descriptor only inside the signed rollback bundle until cutover acceptance; current installers replace it atomically and stale running hosts are rejected until restart.
- [ ] Commit `refactor(root): route host hooks through v2 runtime`.

### PR 25R: API/router/static-shell coexistence

**Files:** modify `src/dashboard/{mod,assets}.rs`, `build.rs`, package include list; create V2 router/static/redirect composition files identified in plans 10–11.

- [ ] Add failing direct-load/history/base-path/CSP/cache/asset/legacy-redirect tests from both Git checkout and packaged crate.
- [ ] Mount V2 API/SSE/SPA and legacy routes with explicit non-overlap.
- [ ] Prove every old writable action still works or is redirected to an equivalent typed command.
- [ ] Commit `feat(root): host v2 api and brain shell beside legacy dashboard`.

Companion PR 24G/24H and plan 17 SDK slices add generated scope/anchor/problem bindings, official API lifecycle commands/packages, and privacy status/scan/remediation bindings over this same server/application. Root owns socket/runtime discovery, keyring/token delivery, process service, and coordinated package publication only. Every V1-backed compatibility response crosses plan 18's sanitizer/output-eligibility boundary; raw legacy dashboard, response-handle, summary, memory-metadata, or backup bytes cannot be served as a shortcut.

### PR 33R: Whole-profile migration controller

**Files:** create `src/migrate/v2/**`, `tests/v2_migration/**`; extend CLI/MCP/API command adapters with operation-specific inspect/plan/start/status/cancel/resume/rollback/archive.

- [ ] Add copied-store fixtures for #405 identity adoption/conflict, #407 Hermes facts-only/session/fact collision, #410 eight-child messages, #411 foreign-skill ownership/remediation, #412 lifecycle/WAL/service-state races, #414 move-symbol inventory, #417 split visibility, merged #441 transitional `user-memory.db` with versions/trust/vectors/feedback/relations plus multiple Hermes host profiles sharing one TraceDecay profile and its live-main-file/reflink versus WAL-checkpoint branch-snapshot race, #443 exact orphaned generated-block recovery and per-session neutral/unresolved legacy-source classifications, #445 fresh install plus 0.0.55→0.0.56 update/reinstall with installed-profile ownership, provider-home reset, host-home project exclusion, registered descendant repositories, and user-scope CLI/MCP bypass, #447 scan-once/semantic-boundary/generation/checkpoint and shared-memory branch fixtures, #448 selected-profile user-search/ambiguous-provider/live-hook/registry-failure/daemon-tree shutdown fixtures, #450 lifecycle handoff/holder/transient-error/service-unavailable/offline migration fixtures, merged #452's full Windows consolidation suite plus scoped test-only offline guard, merged #425 dual-nonempty consolidation (canonical platform paths, same inode through another path, unsupported holder, reservation race, one-of-two backup failure, confirmation drift, every ledger crash state, table reject/rebuild/merge/collision, remapped LCM source edge, failed exhaustive verify, marker/registry atomicity, exact doctor recovery), graph-resident memory including V11 metadata/vectors, corrupt/missing payload, unsafe summary/response-handle/backup descendants, automation artifacts, branch DBs, live WAL, interrupted import, and insufficient disk.
- [ ] Implement the ten phases in Section 14 with immutable receipts/checkpoints.
- [ ] Execute storage-side import transactions only through plan 02's PR 33S/33S-2 importer executor consuming capture-sanitized batches (plan 03 PR 7E); root owns phases, receipts, and operator surfaces only.
- [ ] Kill at every phase boundary and resume; second complete run inserts zero canonical duplicates.
- [ ] Restore backup/archive into a clean profile and run parity corpus.
- [ ] Commit `feat(migrate): orchestrate resumable v1 to v2 migration`.

### PR 33I: Existing-profile remote Brain enrollment and correlation

- [ ] Bootstrap local versus remote authority before store open; enroll the node with protected one-time material; verify `BrainId`, grants, versions, placement, backup, privacy eligibility, and authority epoch before publishing any route.
- [ ] Correlate repositories/checkouts across nodes through plan 16/28 Git proofs; ambiguous forks/shallow/rewritten histories remain candidates and block automatic adoption.
- [ ] Seed replicas/caches only from signed manifests, resume every upload/import phase idempotently, and prove no code path opens a database/WAL over the network.
- [ ] Kill/retry every enrollment, placement, snapshot/tail, receipt, and publication boundary; failed migration leaves the prior local authority unchanged.

### PR 34R: Shadow parity runner and operator surface

**Files:** extend `src/compat/parity.rs`, application operations adapter, Observatory migration/parity views, machine receipts.

- [ ] Compare all bounded contexts at identical captured watermarks, including rankings, paths, message representative views, fact lineage, hint replay, actions, and output shapes.
- [ ] Render unexplained gaps, caps, unavailable shards, live drift, and accepted normalizations; no green aggregate may hide a red domain.
- [ ] Require 24-hour continuous capture/projection/latency/continuity observation where plans specify it.
- [ ] Commit `feat(compat): prove bounded v1 v2 parity`.

### PR 35A–35J: Bounded-context cutovers

**Files:** route receipts/config only plus the exact compatibility adapter whose context changes.

- [ ] For each Section 14.2 context (including 35I/35J in their declared order), verify inventory, backup, parity, load/privacy, host/transport behavior, and no conflicting open-PR seam.
- [ ] Require the plan 18 PR 33A retroactive-privacy-audit remediation/restore-eligibility receipt for the context; zero synthetic canary hits is a per-context cutover precondition.
- [ ] Pass the explicit telemetry gate: delivery/outcome/error/latency counters at the cutover watermark plus the current-client catalog handshake are green before the slice is declared complete.
- [ ] Under the exclusive lifecycle lease, quiesce processes, publish `V2Authoritative`, restart current clients/hosts, observe, drill the explicit operator rollback, resume to a new epoch, then republish V2.
- [ ] Do not delete code/store data in these PRs.
- [ ] Commit one bounded context per PR.

### PR 36R: V2-default release

**Files:** route defaults, release-plz/workflows, docs/changelog, doctor/upgrade messaging, package manifests, signed host-bundle release manifests and native-marketplace publication receipts.

- [ ] Publish implementation crates in dependency order, then root; verify registry checksums and isolated install.
- [ ] Freeze one source commit/catalog/integration-manifest digest; compile every supported host/package unsigned payload twice and require byte-identical `HostBundlePayloadV1` trees, package/component digests, capability/difference/conformance inputs, and release-scan inputs. Then independently rebuild/scan/conformance-test/sign through PR 36R and verify the resulting `HostBundleManifestV1`, licenses/SBOMs, signatures, receipts, supported-host matrix, and deployment-time `ResolvedHostBundleV1` runtime-probe binding.
- [ ] Publish the core/context/work/operator package set atomically through one signed release index, verify native locators against expected digests before promotion, and prove a failed upload leaves the prior release current with no operator-default drift.
- [ ] Default new and migrated profiles to V2 only when signed receipts exist; unmigrated profiles remain V1 with guided preflight.
- [ ] Preserve read-only V1 archives and current-binary restore/reimport tooling only; close migration-mode V1 ownership, remove public V1 routes, retired tool names, and stale protocol/plugin bindings.
- [ ] Run upgrade, daemon skew, dashboard package, backup/restore, and supported-host install/update/repair/uninstall/reload/trust/cloud-vs-local conformance drills; versions/digests and unsupported cells remain visible in the receipt.
- [ ] Commit `release: make v2 the tracedecay default`.

### PR 36S: Protected multi-machine release gate

**Files:** plan-28 conformance/fault fixtures, release receipts, supported transport matrix, backup/restore and RPO/RTO reports.

- [ ] Pass local/authority/client/replica/standby/hybrid, partition, revocation, privacy, cache, Git identity, restore/promotion, old-authority, RPO/RTO, and 10× scale cases over ordinary HTTPS/mTLS; repeat the connectivity subset over optional Tailscale without changing application semantics.
- [ ] Prove standby promotion is impossible on unreachability/time alone and succeeds only after a graceful old-authority fence receipt, verified external exclusive-resource revocation, or expired independent quorum lease term; restore also succeeds after total authority-node/keyring loss through separately wrapped recovery keys.
- [ ] Enable remote mode only for profiles with current enrollment, one fenced authority per shard, verified backup/recovery receipt, explicit placements/privacy classes, and current client/schema/catalog versions. Local-only operation remains complete.

### PR 37A–37L: Retirement

**Files:** exact deletion groups in Section 15; compatibility inventory status updates; archive/deletion receipts.

- [ ] Before each deletion PR, prove no active route/caller/host manifest/replay requires the code, move fixtures to the owner, and run impact/test mapping.
- [ ] Delete only one Section 15 row per PR and rerun full compatibility/archive restore.
- [ ] Remove retired tool/argument aliases at cutover. Keep only human browser navigation redirects and current-binary archive readers for exactly one full release after the owning retirement slice merges — that is the declared data/UI window; when it closes they fail typed with the current replacement.
- [ ] Physical user-store deletion remains an explicit command after code retirement; repository cleanup does not touch profile data.
- [ ] PR 37K removes copied legacy host installers/config fragments only after plan-27 conformance and ownership proof; it preserves foreign caches, unmanaged plugins, user/team/workspace config, unknown keys, backups, and any path whose receipt ownership is missing.
- [ ] PR 37L removes legacy path/store-file remote routing and temporary remote-authority compatibility adapters only after PR 36S; it preserves foreign VPN/proxy/certificate configuration and user-managed infrastructure.

## 18. Verification Matrix and Release Gates

### Architecture and inventory

- `cargo tree` and source lint: no V2 crate imports root, MCP, dashboard, CLI, or V1 modules; no root compatibility type leaks into public V2 contracts.
- Compatibility inventory: 100% owned/dispositioned; zero duplicate/unmapped command/tool/route/action/config/store/provider/operation row.
- Checked `architecture-boundaries.toml` plus its generated DAG/owner/release/deletion reports and plan-19 reuse/footprint reports: <=11 Rust packages including root/client; hook/presentation/API/remote-Brain-transport remain root-private modules; zero forbidden edge, unowned duplicate infrastructure engine, or package-admission drift.
- Every parity replacement has a checked deletion receipt and is net-negative in handwritten production code after its V1 path/adapter retires; generated lines, dependencies/features, tables/indexes, workers, artifacts, binary/RSS/startup/build time, and stored bytes are reported separately.
- Production file size: new bounded modules target <=400 lines and remain <=800 lines absent a temporary plan-19 waiver; root service graph <=500 lines.
- Generated catalog/OpenAPI/client/plugin/dashboard artifacts and canonical/resolved host-bundle release trees: deterministic and clean after two runs; every package/component digest, signature, SBOM/license row, capability/difference/conformance report, supported-host cell, and marketplace locator reconciles to one signed PR 36R receipt.

### Data and migration

- Second import: zero additional canonical observations/entities/events/relations/blobs.
- Counts/hashes/offsets/ordinals/timestamps/aliases/LCM ranges/fact versions/skill versions/artifacts reconcile or have named quarantine.
- #405 moved/symlink/linked/detached/pristine/conflict fixtures preserve identity and block ambiguity.
- #407 ordinary-profile and facts-only fixtures preserve provenance/collisions without a Hermes profile.
- #410 raw/native/direct/subagent/tool-result/representative/hidden-copy fixtures match across all transports.
- Graph-resident memory gate: every fact/entity/version/trust/feedback/deletion link verified before branch DB archive.
- Backup/restore and archive restore run in clean profiles; allocation IDs and query results remain stable.
- Kill/restart at every migration/store/spool/outbox/route/archive boundary yields complete commit or safe retry.

### Process and transport

- Hook and one-shot commands open no unnecessary broad service/store; daemon owns writers; stale epochs are fenced.
- CLI help/parse/output/error/exit/JSON/dry-run compatibility passes.
- MCP tool/schema/annotation/markdown/JSON/response-handle/degraded/multi-client compatibility passes.
- HTTP auth/Host/Origin/CSRF/CSP/rate/body/deadline/static/history/SSE reconnect compatibility passes.
- Dashboard old routes/actions and new routes direct-load from Git and packaged assets.
- Provider install/update/uninstall/backup/restore/doctor passes for every supported host.
- Daemon startup/recovery/readiness/version-skew/drain/shutdown tests pass within deadline.

### Required commands before each cutover/release

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --no-fail-fast
cargo test --test session_suite
cd dashboard && npm ci && npm run build && npm test
cargo package --locked
```

Also run focused crate, transport, provider, migration, load, privacy, crash, and package-install commands named in plans 01–11. `cargo package --locked` is followed by installing the produced/root registry package in an empty Cargo home and running version, doctor, MCP initialize/list-tools, dashboard asset, and migration-preflight smoke tests.

### Performance and reliability

- Hook notification p95 <=10 ms, prompt/pre-tool p95 <=25 ms, with the plan 07 p99/deadline limits.
- 100 concurrent agents at 1,000 events/s for 10 minutes: zero unexplained canonical loss/duplicate, per-source order preserved, bounded queue/disk behavior.
- Projected visibility p95 <=2 s after drain for 24 hours before affected cutovers.
- One-shot startup and MCP initialize do not regress the recorded V1 p95 by more than 10% before V2-specific work begins; any accepted increase has a measured owner and budget.
- Daemon memory/FD/WAL/spool/worker count remains bounded at current and 10x corpora.
- Secret corpus and every named plan 18 bypass canary yield zero secret-bearing catalog/index/vector/fact/summary/metric/log/error/response-handle/backup/fixture/export/API/SDK/dashboard/package artifact; privacy status has complete policy/source/sink/detector/legacy evidence rather than a lossy-row boolean.

## 19. Rollback and Blocker Rules

An execution slice is blocked, not waived, by:

- unresolved identity split or missing #405 receipt;
- #425/V2 reconciliation plan without both immutable verified backups, complete table/collision/remapped-edge dispositions, holder/reservation proof, restartable ledger state, or confirmation revalidation before marker/registry cutover;
- missing #407 facts-only/collision evidence or accidental Hermes profile creation;
- irreversible #410 row collapse or lost sanitized-native expansion;
- unexplained parity gap, dead letter, source gap, corrupt archive, missing allocation ledger, or hash mismatch;
- graph-store deletion proposal without memory/fact lineage proof;
- effect duplication in hook/automation/mutation shadow;
- schema/catalog major mismatch without a read-only diagnostic path;
- package that cannot install from crates.io without workspace/Git assets;
- provider installer that overwrites unrelated user configuration;
- plain libtest/nextest disagreement after PR 3R;
- open PR/master/index drift that changes an owned seam without an updated receipt.

Rollback never deletes observations, accepted V2 stores, or diagnostic evidence. It restores route/effect ownership and source positions from the last signed receipt, fences stale writers, retains the failed generation, and records the rollback as a canonical operational event.

## 20. Definition of Done

- The published `tracedecay` command, daemon, MCP identity, provider integrations, and upgrade path remain stable through the strangler program.
- Every V1 behavior and artifact has one generated inventory row, V2 owner, compatibility status, test, and deletion criterion.
- Root composition is process-specific, lazy, typed, and free of domain business/storage/query/policy logic.
- All canonical writes, reads, commands, hooks, automations, and product routes are V2-authoritative with proven rollback and explicit coverage.
- CLI, MCP, HTTP, dashboard, hooks, provider installers, exports, saved views, and labs share catalog/application semantics while preserving declared compatibility.
- #405 identity adoption, #407 ordinary-profile Hermes consolidation, #410 complete sanitized-native/representative message semantics, #411 foreign-skill ownership, #412 lifecycle fencing, #413/#416/#418/#427/#429/#431/#433/#437/#442/#444/#446/#449/#451 release metadata, #414/#419 race-safe move-symbol semantics, #415 release integrity, #417 identity-split visibility, #420 proxy-before-store/no-write-replay semantics, #422 generation-scoped catalog refresh, #423 FTS/counter semantics, #424 aggregate-before-sample analytics, #425 split-store consolidation, #426 untracked branch-graph recovery, #428 divergent session variants, #430 indexed family lookup, #432 hook lifecycle quiescence, #434 conflict-safe registry reconstruction, #435 explicit FTS maintenance, #436 no-mmap peer checkpoint safety, #438 applied-manifest retirement, #439/#440 per-manifest doctor/registry reconstruction truth, #441 Hermes memory/context routing, #443 post-update recovery, #445 projectless host routing, #447 catch-up/integrity hardening, #448 user-scope/routing/shutdown hardening, #450 lifecycle/Windows migration recovery, and #452 Windows consolidation coverage are present in accepted base `81fe404c`; #409 remains historical only.
- `tracedecay.db` and every V1 sidecar/store are archived and restore-tested; graph-resident durable facts are migrated before any graph DB deletion.
- Giant V1 modules are removed by bounded deletion PRs after callers, replay, tests, release windows, and archives prove they are unnecessary.
- Final workspace stays at or below 11 Rust packages; no root-only adapter was published; the canonical registry/encoder, projection runtime, operation/scheduler kernels, host manifest, graph/timeline slice pipeline, saved-view lifecycle, and presentation pipeline each have one implementation and deletion evidence for their V1 duplicates.
- Every parity-replacement lane is net-negative handwritten code and passes the plan-19 dependency/runtime/build/data footprint gates; a wrapper around live V1 code does not count as deletion.
- V2 implementation crates and root publish/install in dependency order from crates.io; upgrade, daemon skew, backup, restore, rollback, and host refresh pass.
- Full nextest, plain `session_suite`, dashboard, package, privacy, crash, concurrency, parity, archive-restore, and user-task gates pass with zero unexplained gap.
- V1 physical data is deleted only by an explicit retirement `plan` followed by a separately authorized, receipt-bound `start` command after compatibility and archive windows close.

## 21. Plan Self-Review Checklist

- [ ] Refresh `origin/master`, every accepted row in Section 3, open #421 state, merge bases, direct files, checks, and TraceDecay semantic snapshots; record #409 as historical only.
- [ ] Verify PR suffixes do not conflict with plans 01–11 or master PR 1–37 ordering.
- [ ] Verify every `src/lib.rs` module, root binary command module, dashboard plugin, provider integration, config source, store/sidecar, and release artifact appears in inventory ownership.
- [ ] Verify each create/change/delete path has one phase, owner, failing test, cutover gate, rollback, and deletion criterion.
- [ ] Verify no V2 crate imports root/V1 and root compatibility contains no new business logic.
- [ ] Verify source-family authority never advances independently on both V1 and V2.
- [ ] Verify `tracedecay.db` durable memory/facts are explicitly migrated before branch graph deletion.
- [ ] Verify the order-dependent `session_suite` baseline has a fresh-process contract and is not used as a blanket waiver.
- [ ] Verify workspace publication works from crates.io without local path/Git/dashboard build assumptions.
- [ ] Verify PR 36R publishes one signed, component-atomic host release set from the canonical manifest/catalog and PR 37K deletes only receipt-owned legacy material while preserving every foreign cache/config/backup byte.
- [ ] Run the placeholder scan with split regex atoms and resolve every match before implementation handoff.
