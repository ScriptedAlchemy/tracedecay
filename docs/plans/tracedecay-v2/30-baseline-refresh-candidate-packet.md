# TraceDecay V2 baseline-refresh candidate packet

> Candidate packet for owner review only. No production behavior is changed by
> this document. Its authoritative evidence base is
> [`29-baseline-delta-audit.md`](29-baseline-delta-audit.md); this packet
> operationalizes that audit into normative planning deltas and does not restate
> or re-derive unverified claims. Where this packet and the canonical audit
> appear to conflict, the audit's labeled **[FACT]**/**[CONTRACT]** evidence
> governs and this packet must be corrected.

## 1. Purpose and standing

This packet is the first-pass author's proposal to refresh the accepted V2
normative baseline from the v0.0.58 endpoint through the exact verified
implementation master and the current design HEAD, and to distribute the audit's
dispositions across every affected owner plan, the compatibility/migration plan,
the failure matrix, the architecture record, and the regression-fixture surface.

It exists so that owners can accept, amend, or reject one coherent set of
changes rather than reconcile a raw audit against 30 large plan documents by
hand. Acceptance of this packet is not implied by its existence: §8 preserves the
audit's unresolved contradictions as explicit owner decisions, and none of them
may be treated as silently resolved.

## 2. Immutable endpoints and re-verified facts

The endpoints below are copied from audit §1 and independently re-verified for
this packet by this session's Git range checks (see §10 for the receipt).

| Symbol | Commit | Role |
|---|---|---|
| `B` | `81fe404c00bfa1b6a3d1e33a9b3da61d77025cc4` | Accepted baseline; PR #451 merge (parents `fc89e8be`, `c5625c9e`); shares v0.0.58's tree. |
| `M` | `e560005610ac296018c3a16b9e6bded90de0eff5` | Accepted implementation endpoint; merge PR #462; v0.0.63 release content `0532c767`. |
| `D` | `f18f0f14b3e7e2da30eefd9f1ed88862c0d73e57` | Design endpoint; current checkout HEAD; `fix(architecture): enforce daemon-owned physical writers`. |

Re-verified this session (matches audit §1 exactly):

- `B` is an ancestor of `M`; `M` is an ancestor of `D`.
- `git rev-list --count B..D` = **88** (89 including `B`).
- `git diff --shortstat B D` = **125 files changed, 41,778 insertions(+), 286
  deletions(-)**.
- `git diff --name-status -M B D` reports **0** renames.
- `git diff B D -- src/daemon.rs` is **net-zero** (0 lines), confirming the
  shutdown-timeout add/revert pair (`246427a6` then `42d7ce8e`).

`B^..D` remains forbidden for range counting: because `B` is a merge it admits one
second-parent-only commit and yields the misleading count 90. Endpoint-tree
comparisons use `git diff B D`.

## 3. Refreshed normative base manifest/index

The accepted base is refreshed to span `{B} ∪ (B..D)` in two segments. The
[plan-set index](00-plan-set-index.md) "Accepted-base refresh" note and
[plan 12 §4](12-root-compatibility-migration.md) baseline table are the tracked
homes of this refresh; this section is the canonical rationale both cite.

### 3.1 Implementation segment `B..M` (runtime + release)

Sixteen merges are verified in `git log --merges B..M`: ten PR merges and six
design-branch reconciliations with `origin/master`. The PR merges are:

| PR | Merge | Disposition | Rationale |
|---|---|---|---|
| #453 | `8001a1f4` | **Preserve** | Runtime/CI hardening, Hermes projectless-compression routing (`cursor`→`hermes` LCM provider), registry/alias-aware session-project resolution, cross-scope Turn correlation, fixture normalization, dogfood launch fixes. Owners: capture/store/scope/transport/provider. Audit §6.1. |
| #454 | `b1a3a13f` | **Release-only** | v0.0.59 package/tag baseline; normative only through its source/tests, never the version bump. Audit §5.7. |
| #455 | `41b2bdd4` | **Preserve** | Deferred exclusive-maintenance vacuum for live memory, replay identity during Hermes compression, compact hook routing, bounded daemon shutdown/teardown, installed-upstream Hermes layout, CLI fallback docs. Owners: store/capture/scope/host-lifecycle. Audit §6.2–§6.4. |
| #456 | `655296e4` | **Release-only** | v0.0.60 package/tag baseline. |
| #457 | `a01ac4d9` | **Preserve** | Managed-skill materialization fix; export isolation to default profile. Owners: automation/store/migration. Audit §6.5. |
| #458 | `2f3fac96` | **Release-only** | v0.0.61 package/tag baseline. |
| #459 | `227fad0b` | **Preserve** | Managed-skill export isolation and foreign-ownership protection (`ManagedSkillSource` rejection of non-`AutomationRun`). Owners: automation/store/migration. Audit §6.5. |
| #460 | `313d84c1` | **Release-only** | v0.0.62 package/tag baseline. |
| #461 | `ab983634` | **Preserve** | Safe upgrade shutdown-progress messaging (`src/update_cmd.rs`); lifecycle/release fixture, adds no interrupt guard. Owners: root-lifecycle/migration. Audit §6.3. |
| #462 | `e5600056` | **Release-only** | v0.0.63 package/tag baseline; the accepted implementation endpoint `M`. |

The six reconciliation merges are explicitly dispositioned as **provenance
only**: `2208969d`, `9561cc30`, `0dc1ee14`, `363de8c7`, `859e5ed3`, and
`f1e2ec32`. They record repeated integration of `origin/master` into the design
branch; they add no independent contract beyond the runtime, test, release, and
surviving-plan changes dispositioned here and in audit §5. Each remains in the
ancestry receipt so its carried commits cannot be silently omitted.

Merge-name, host-layout, or provider-specific fixes in this segment do **not**
become V2 architecture by appearing here. Each surviving runtime behavior must be
preserved or explicitly superseded by its owner before implementation, with the
dispositions in §5.

### 3.2 Design segment `M..D` (plan corpus + architecture governance)

`M..D` is **entirely documentation/plan/governance work**: 47 non-merge commits
and two foundation merges (`2f029bbb`, `e3199ed3`), no production runtime source outside
generated architecture-governance artifacts. Verified: every `M..D` changed path
falls in `docs/plans/tracedecay-v2/**`, `docs/architecture/v2/**`,
`architecture-*.toml`, `tests/architecture_boundaries.rs`, corpus fixtures, or
`.codex`/`.claude` skill scaffolding (audit §5.3–§5.6).

Blanket disposition for `M..D`: **supersede intermediate plan drafts with the
surviving files at `D`; retain merge and draft commits as provenance.** The
numbered plans `00`–`28` plus the architecture hierarchy at `D` are the surviving
normative text; earlier drafts in the same commit lineage are non-authoritative
once superseded. Notable design commits given explicit disposition:

| Commit | Subject | Disposition |
|---|---|---|
| `f18f0f14` (`D`) | enforce daemon-owned physical writers | **Preserve as governance.** Sets all five store entries to `physical_writer = "store"` plus semantic producers, machine-enforced by `tests/architecture_boundaries.rs`. Audit §6.3. |
| `4e356a53`, `65c029c9`, `98a22fad` | enforce materialized/machine V2 boundaries; restore verification artifacts | **Preserve as governance.** `architecture-boundaries.toml`, `architecture-dependency-policy.toml`, generated views, and `scripts/generate_architecture_views.py` are the reproducible governance layer. |
| `fd9c7790`, `ee303773`, `2821c52f`, `f51367cd` | execution lifecycle contracts; canonical execution-graph activation; mixed Claude/Sol execution | **Preserve.** Plan-24 execution/PR-heading contracts and the `.codex/executing-tracedecay-v2-plan` helper. |
| `1cda1b2a`, `865ff0ab` | add / simplify V2 redacted corpus foundation | **Preserve as evidence.** `tests/fixtures/v2/**` + `tests/v2_corpus_suite/**`; non-production but normative identity/provider corpus. Audit §5.5. Does not prove current host-hook event mappings. |
| `535b4a88`, `649f7062`, `f1bedb93` | separate evidence from product authority; dashboard build authority | **Preserve.** Keeps generated views reviewable-projection, not product authority. |
| `71694e86` … `c1d465b5` | draft/reconcile/deepen the V2 plan set | **Superseded by `D`.** Retained as provenance only; the surviving numbered files govern when text conflicts. |

## 4. Merge/commit disposition ledger

Every intervening merge and commit receives an explicit disposition and
rationale. Rather than duplicate the audit's per-file evidence, this ledger
references it:

- **`B..M` PR merges (#453–#462):** dispositioned individually in §3.1;
  evidence in audit §4 (common implementation history) and §6.
- **`B..M` reconciliation merges (6):** dispositioned individually by SHA in
  §3.1 as provenance-only integrations.
- **`M..D` design commits (47 non-merge + 2 merges):** dispositioned in §3.2;
  the surviving-file-governs rule (audit §4 "supersede intermediate plan
  drafts") applies uniformly; per-subsystem evidence in audit §6.
- **Release-only commits and merges** (`CHANGELOG.md`, `Cargo.toml`,
  `Cargo.lock`, `.changeset/*`): **excluded as independently normative.** They
  are publication inputs only (audit §5.7); behavior is normative solely through
  the corresponding source and tests.
- **Foundation merges** (`2f029bbb`, `e3199ed3`): **preserve as governance and
  evidence**, respectively, as detailed in §3.2; their merge topology is also
  retained as provenance.

No commit in `B..D` is left without a disposition. The complete
125-path changed-file inventory and its classification live in audit §5 and are
not duplicated here.

## 5. Owner-plan dispositions (delta pointers)

Each affected owner plan carries a compact "Accepted-base refresh delta (audit 29
/ packet 30)" pointer to this section. The normative dispositions, drawn from
audit §6 and §9:

| Owner plan | Required action |
|---|---|
| [02 store](02-store-crate.md), [19 convergence](19-system-defragmentation-convergence-and-extensibility.md) | Preserve daemon-owned physical writers and deferred live vacuum; **add** an explicit periodic exclusive-maintenance cadence and reclamation receipts, independent of upgrades (audit §6.3, §8.3). |
| [04 projectors](04-projectors-crate.md), [16 scope](16-cross-project-repository-worktree-scope.md), [23 session/LCM](23-session-lcm-temporal-retrieval-and-evaluation.md) | Preserve user-canonical plus per-project projections; **add** per-shard idempotent receipts and catch-up reconciliation so "searchable from every touched repo" survives a mid-loop shard failure (audit §6.1, §8.2). |
| [07 hooks](07-hooks-crate.md), [27 bundles](27-cross-host-agent-plugin-bundles.md) | Preserve verified current host asymmetries (Codex six-hook `config.toml` + trust table; Claude seven-entry bundle with unique `PostToolUseFailure`; absent Codex `PreToolUse`; Hermes in-process callbacks) until deliberately migrated; **decide** the hook-trust-state owner and restore a compact Codex parent-owns-writes token (audit §6.6, §8.5–§8.6). |
| [09 application](09-application-crate.md), [24 executor](24-canonical-task-plan-graph-and-multi-agent-executor.md) | Make retries and shutdown deadlines explicit bounded state-machine transitions, not best-effort prose; **add** a compression-path oversize-retry ceiling/terminal disposition and an end-to-end bounded-shutdown contract (audit §6.2, §6.4, §8.1, §8.10). |
| [12 migration](12-root-compatibility-migration.md) | Specify `user-turn-v2` resweep budget/resumability, `cursor`→`hermes` provider continuity, orphan v1-cursor policy, and notification cardinality (audit §7.5–§7.7, §8.7–§8.9). Refreshed in plan 12 §4. |
| [26 observability](26-observability-accounting-and-usage.md) | Distinguish, as separate observable outcomes: skipped export, partial projection, retry exhaustion, runtime-drop timeout, and async shutdown timeout (audit §9, §8.1–§8.3). |
| Architecture [storage-and-consistency](../../architecture/v2/storage-and-consistency.md) | Do **not** mark "deterministic shutdown/checkpoint" satisfied until a never-resolving-task process-exit test passes (audit §6.4, §8.1). |

## 6. Compatibility and migration planning updates

These deltas land in [plan 12 §4](12-root-compatibility-migration.md) as the
post-baseline refresh table and are summarized here for review coherence:

1. **`user-turn-v2` resweep (audit §7.5, §8.8).** `src/sessions/hermes.rs` bumps
   `user-turn-v1`→`user-turn-v2` and removes registered-project exclusion from
   the canonical user sweep. Migration must specify a resweep CPU/IO/storage
   budget, prove interruption/resumption idempotency, and define explicit
   disposition of orphaned v1 cursor rows (`skipped` reason `unavailable`, or an
   explicit cleanup). This is an expected upgrade cost, not a defect, but it is
   unbounded by contract today.
2. **Provider continuity `cursor`→`hermes` (audit §7.6, §8.7).** The LCM provider
   default changed from `cursor` to `hermes`; historical Hermes records labeled
   `cursor` are not remapped. Migration must either backfill continuity or make
   the split explicitly queryable as two eras. Provider-filtered analytics
   otherwise silently split.
3. **Multi-shard projection reconciliation (audit §7.2, §8.2).** One turn is
   projected sequentially to the user store and every touched project root
   (`for project_root in [None, *project_roots]`), fail-open per root. Migration
   and projectors must add per-shard durable receipts and catch-up so a partial
   fan-out failure is later reconciled with the same message ID and no duplicate.
4. **Notification cardinality (audit §7.7, §8.9).** A turn now emits one
   notification per user/project destination (`1 + unique_project_roots`).
   Migration/observability must pin the count, dedupe, and partial-failure
   behavior; downstream cardinality changes are currently unbounded by contract.
5. **Managed-skill export path safety (audit §7.1, §8.4).** `uses_default_user_
   profile` compares a profile root with `home.join(".tracedecay")` while the
   configuration path can canonicalize a symlinked parent; a symlinked `$HOME` or
   relocated `TRACEDECAY_DATA_DIR` can silently no-op host exports. Migration must
   canonicalize both sides (or compare stable identities) and return an explicit
   intentional-skip receipt.

## 7. Regression fixture specifications

Each fixture below is a review specification, not executable code. Every fixture
names setup, action, assertions, and failure coverage, and maps to the audit's
required scenarios (§10) and to the failure matrix rows added in
[plan 14 §7.6](14-historical-failure-regression-matrix.md) (FM-161–FM-171).

### 7.1 Runtime routing — multi-shard projection (FM-162; audit §10.2)

- **Setup:** a user profile plus ≥2 registered project roots; one Hermes turn
  that touches both projects; a fault injector that fails the second project
  shard's projection after the user store commits.
- **Action:** project the turn through `[None, *project_roots]`; then run the
  reconciliation/catch-up pass.
- **Assertions:** user store and first project advance; the failed shard is later
  reconciled with the identical message ID; no duplicate row in any shard;
  `associated_project_roots` preserved through the MCP live projection.
- **Failure coverage:** mid-loop shard failure, restart between shards, repeated
  reconciliation (idempotent), and a shard permanently unavailable (explicit
  partial-coverage receipt, never silent success).

### 7.2 Compression + live-memory writes (FM-164, FM-168; audit §10.6, §10.7)

- **Setup:** an LCM replay stream containing messages with non-empty
  `lcm_summary_node_id`; a live `MemoryStore` with peer connections open; an
  oversized memory-curator input exceeding the character/token limit.
- **Action:** replay compression; delete facts repeatedly; submit the oversized
  input; then run scheduled exclusive maintenance.
- **Assertions:** replayed summarized messages bypass raw re-ingest (identity
  precedence: explicit ID → store-ID lookup → deterministic fallback); no
  duplicate raw messages; `remove_fact` defers vacuum (non-empty freelist, peers
  usable, no immediate shrink); scheduled exclusive maintenance reclaims pages
  within a declared bound; oversized input terminates after a bounded retry
  policy with an explicit terminal disposition.
- **Failure coverage:** id-less replay (no re-ingest), high fact churn between
  maintenance windows, irreducible oversized input (no infinite Retryable loop),
  and a peer holding a live connection during deferral.

### 7.3 Managed-skill ownership (FM-161, FM-170; audit §10.1)

- **Setup:** a default-profile home and a symlinked `$HOME`/relocated
  `TRACEDECAY_DATA_DIR`; managed skills from `AutomationRun`, `UserDraft`, and
  `Import` sources.
- **Action:** run export/materialization and an automation update/consolidation.
- **Assertions:** symlinked/relocated home still exports managed skills **or**
  returns an explicit intentional-skip receipt (never a silent no-op);
  `UserDraft`/`Import` sources are rejected from automation overwrite; the
  default-profile predicate canonicalizes both sides.
- **Failure coverage:** symlinked parent, relocated data root, non-default profile
  root, and a foreign-owned skill an automation run tries to overwrite.

### 7.4 Daemon shutdown (FM-163; audit §10.3)

- **Setup:** a daemon with an injected background task whose outer
  `shutdown_background_tasks().await` never resolves.
- **Action:** request process shutdown.
- **Assertions:** the process still exits within the declared deadline; a
  never-resolving async future cannot prevent `run(cli)` from returning past that
  bound; the `runtime.shutdown_timeout(2s)` runtime-drop bound is proven to apply
  only after `run` returns, not as a rescue for a stuck outer await.
- **Failure coverage:** never-resolving outer await, late task spawn during
  shutdown, and a bounded-versus-unbounded distinction asserted end-to-end (not
  from `src/daemon.rs` net-zero alone). This fixture is distinct from FM-157
  (process-tree containment); it targets the async-await ceiling specifically.

### 7.5 Hermes/Codex/Claude hooks (FM-169, FM-170; audit §10.9, §10.10)

- **Setup:** the current installed host bundles for Codex, Claude, and Hermes.
- **Action:** install/register hooks; render Codex steering; enumerate host
  event mappings.
- **Assertions:** host-hook registration, dispatch, event mapping, error
  handling, and uninstall are byte-identical to `B` (only `src/hooks/steering.rs`
  changed in `src/hooks/**`); Codex steering retains all eight agent names plus a
  compact parent-owns-writes token; a contract test pins Claude-only
  `PostToolUseFailure`, the absent Codex `PreToolUse`, Codex `config.toml`
  registration/uninstall, and the **chosen** trust-state owner.
- **Failure coverage:** Codex trust-table write versus plan-07 "bundle does not
  edit trust state" contradiction surfaced as a decision gate (not silently
  resolved); Hermes Python callbacks correctly classified as a provider bridge,
  not OS hooks.

### 7.6 Storage fixes (FM-165, FM-166, FM-167; audit §10.4, §10.5, §10.8)

- **Setup:** a v0.0.58 store with `user-turn-v1` cursor data and historical
  provider records labeled `cursor`; multi-project turns.
- **Action:** upgrade to `user-turn-v2`; run analytics fan-out; emit
  notifications.
- **Assertions:** v1 data is fully and idempotently reswept into v2, including an
  interrupted/resumed run and explicit old-cursor disposition; provider continuity
  across `cursor`/`hermes` is either migrated or intentionally queryable as two
  eras; notification count is exactly `1 + unique_project_roots` with dedupe and
  pinned partial-failure behavior.
- **Failure coverage:** interrupted resweep, orphaned v1 cursor rows, provider-
  filtered analytics split, and notification multiplication under many project
  roots.

## 8. Unresolved issues carried forward

The audit's §8 contradictions remain owner decisions. This packet does **not**
resolve them; it routes each to an owner and a fixture. A refreshed baseline may
not be declared while any item is silently assumed resolved.

| # | Unresolved issue | Owner(s) | Fixture / FM |
|---|---|---|---|
| U1 | Shutdown-determinism overclaim vs unbounded outer async await | 09, 24, arch storage-and-consistency | §7.4 / FM-163 |
| U2 | Missing multi-shard reconciliation after partial fan-out | 04, 16, 23 | §7.1 / FM-162 |
| U3 | Missing periodic reclamation cadence for deferred vacuum | 02, 19 | §7.2 / FM-164 |
| U4 | Canonical-path contradiction in default-profile detection | automation/02/12 | §7.3 / FM-161 |
| U5 | Hook-trust contradiction: plan 07 vs Codex installer | 07, 27 | §7.5 / FM-170 |
| U6 | Hook target/current mismatch treated as implemented | 07, 27 | §7.5 |
| U7 | Provider migration `cursor`→`hermes` omitted | 12, 23 | §7.6 / FM-166 |
| U8 | Cursor migration: no resweep budget/resumability/cleanup | 12 | §7.6 / FM-165 |
| U9 | Notification cardinality unbounded by contract | 12, 26 | §7.6 / FM-167 |
| U10 | Oversize retry termination unspecified (compression path) | 09, 06 | §7.2 / FM-168 |
| U11 | Test-execution limitation: contracts source-inspected, not a passing suite | all owners | §10 gate |
| U12 | Endpoint drift: any commit after `D` re-opens the audit | packet author | re-audit gate |

**U11 is explicit:** this packet executed only the focused 10-test architecture
boundary suite; the audit did not execute the full Rust suite. The other named
contracts are source-of-truth for encoded behavior, not proof of a passing run.
Aggregate verification
(`cargo nextest run --workspace --no-fail-fast` with an isolated
`TRACEDECAY_DATA_DIR`) is required before owner acceptance.

## 9. Cross-document reference map

- This packet ← audit [29](29-baseline-delta-audit.md) (evidence base).
- [00 index](00-plan-set-index.md) "Accepted-base refresh" → this packet §3 and
  audit 29; registers docs 29 and 30 in the plan table.
- [12 §4](12-root-compatibility-migration.md) baseline table → this packet §3, §6
  and audit 29 for the post-baseline `B→M→D` refresh.
- [14 §7.6](14-historical-failure-regression-matrix.md) FM-161–FM-171 → this
  packet §7 and audit §7–§8.
- Owner plans 02, 04, 07, 09, 16, 19, 23, 24, 26 → this packet §5 pointer block.
- Architecture [storage-and-consistency](../../architecture/v2/storage-and-consistency.md)
  shutdown caveat → this packet §5, §7.4 and audit §8.1.

## 10. Validation receipts

Documentation-focused validation performed for this packet (this session):

- **Git range re-verification** (§2): ancestry `B<M<D`, `B..D` = 88 commits,
  `git diff --shortstat B D` = 125 files / +41,778 / −286, 0 renames,
  `src/daemon.rs` net-zero. All match audit §1/§3.
- **Merge topology** (§3): `git log --merges B..M` yields exactly ten PR merges
  (#453–#462) plus six `origin/master` reconciliation merges; `git log --merges
  M..D` yields exactly the two foundation merges `2f029bbb` and `e3199ed3`.
- **`M..D` content class:** all `M..D` changed paths confined to docs/plans,
  architecture, governance TOMLs, `tests/architecture_boundaries.rs`, corpus
  fixtures, and skill scaffolding — no independent runtime source.
- **`git diff --check`** run over the working tree before hand-off (no whitespace
  errors introduced).
- **Plan tooling:** `test_plan_inventory.py` and `test_plan_execution.py` pass
  (3 + 3 tests); moving refresh notes after line-number-sensitive headings keeps
  the checked inventory stable.
- **Architecture:** `scripts/generate_architecture_views.py --check` passes and
  `cargo test --test architecture_boundaries` passes all 10 tests.

The full Rust workspace suite was **not** executed (U11); this is a historical
documentation synthesis, and aggregate verification remains an owner-acceptance
gate.
