# TraceDecay V2 normative baseline delta audit

> Review artifact only. No production behavior is changed by this document.

## 1. Scope, immutable endpoints, and evidence labels

This audit covers the source state represented by the accepted v0.0.58 baseline
through the exact post-PR-#452 design endpoint:

- **[FACT] Baseline merge (`B`):** `81fe404c00bfa1b6a3d1e33a9b3da61d77025cc4`.
  It is the two-parent PR #451 merge with parents `fc89e8be0019ca365c3276c39579760378659608`
  and `c5625c9ebf5e152bfc1e71f415e0418e8efa547b`.
- **[FACT] v0.0.58 tag:** annotated object
  `c204ac208b155277e8f97bfdccbdef6913038378`, peeling to `c5625c9e`.
  `c5625c9e` and `B` have the same source tree, but are different Git objects.
- **[FACT] PR #452 integration:** `fc89e8be0019ca365c3276c39579760378659608`
  is already the first parent of `B`; PR #452 is inherited baseline context, not a
  post-baseline delta.
- **[FACT] Accepted implementation endpoint (`M`):**
  `e560005610ac296018c3a16b9e6bded90de0eff5` (`master`, merge PR #462).
- **[FACT] Audited source/design endpoint (`D`):**
  `f18f0f14b3e7e2da30eefd9f1ed88862c0d73e57`,
  `fix(architecture): enforce daemon-owned physical writers`. Later packet and
  remediation commits are post-`D` review artifacts, not members of this range;
  they must identify themselves separately rather than redefining `D` as HEAD.
- **[FACT]** `M` is an ancestor of `D`. The full audit range is `{B} ∪ (B..D)`:
  88 commits strictly after `B`, plus `B` itself, therefore 89 commits.
- **[FACT]** `git diff B D` contains 125 changed paths, 41,778 insertions, 286
  deletions, and no renames.

`B^..D` is intentionally forbidden for counting this range. Because `B` is a
merge, it admits one second-parent-only commit in addition to `B`, producing the
misleading count 90. Endpoint-tree comparisons use `git diff B D`.

Material claims below are labeled **[FACT]** (direct Git/source/test-body
observation), **[CONTRACT]** (tracked normative text or machine-enforced
contract), or **[INFERENCE]** (risk/disposition derived from facts). A test body
is evidence of an encoded contract, not evidence that the behavior passed,
unless execution is stated explicitly.

## 2. Independent evidence trail and limitations

Two independent native-Claude Code 2.1.207 / Opus audit lanes were preserved:

- runtime, compression, memory, and storage: worker session
  `20260712_074618_3ba0b1`, corrected artifact SHA-256
  `426eb4c24a90eb4286a0508ab35b9799fffb1080e8ce7889a9f0f7345c6e1388`;
- skills, shutdown, and hooks: worker session `20260712_074619_09ca9b`, primary
  artifact SHA-256 `f155079ca6301242be13174e6237a401b58b74bdbfb4e930b85f7f0e7cc3cd5b`
  and corrective addendum SHA-256
  `364e9b31d7117b4910cb309ad928794e103cd774b7d0159f95ecbde0537987db`.

Both lanes used native Claude in read-only mode with Bash/Git, Read, Grep, and
Glob. Neither used TraceDecay graph/MCP evidence. The first lane inspected but
did not execute tests. The second lane reported five focused passing lib tests,
but did not run the full workspace or integration suites. This synthesis
re-ran Git range, topology, path-count, shortstat, rename, daemon net-diff, and
host-hook net-diff checks; it did not run Rust tests because this is a
historical documentation synthesis rather than a production implementation.

The primary hooks report initially omitted proof that host-hook mechanisms were
unchanged; its corrective addendum supplies that evidence. The two lanes also
disagreed about shutdown safety. Repository control flow resolves the issue:
`runtime.shutdown_timeout(2s)` in `src/main.rs` executes only after `run(cli)`
returns, while `run(cli)` can still await `shutdown_background_tasks().await`.
Therefore runtime-drop is bounded, but an async task that prevents `run` from
returning is not bounded by that call.

## 3. Reproducible command log

Run from repository root:

```sh
B=81fe404c00bfa1b6a3d1e33a9b3da61d77025cc4
M=e560005610ac296018c3a16b9e6bded90de0eff5
D=f18f0f14b3e7e2da30eefd9f1ed88862c0d73e57

git show -s --format='%H parents=%P tree=%T subject=%s' "$B" "$M" "$D"
git rev-parse v0.0.58 'v0.0.58^{}'
git merge-base --is-ancestor "$B" "$M"
git merge-base --is-ancestor "$M" "$D"
git rev-list --count "$B..$D"                    # 88
git rev-list --count "$B^1..$B^2"                # 1; why B^ is unsafe
git diff --shortstat "$B" "$D"                   # 125 files, +41778/-286
git diff --name-status -M "$B" "$D" | grep -c '^R' || true  # 0
git diff --name-only "$B" "$D"                   # manifest in §5
git diff "$B" "$D" -- src/daemon.rs | wc -l     # 0

git show 246427a6 -- src/daemon.rs
git show 42d7ce8e -- src/daemon.rs src/main.rs

git diff --name-only "$B" "$D" -- \
  plugin/hooks src/hook_cmd.rs src/hooks/claude.rs src/hooks/codex.rs \
  src/hooks/cursor.rs src/mcp/hook_events.rs src/agents/claude.rs \
  src/agents/codex.rs src/agents/cursor.rs          # empty

git diff "$B" "$D" -- src/hooks/steering.rs
git diff "$B" "$D" -- src/agents/hermes/templates/plugin_init.py
git diff "$B" "$D" -- src/memory/store.rs src/sessions/lcm/compression.rs
```

## 4. Merge history and proposed disposition

- **[FACT] Common implementation history (`B..M`)** is 38 commits: 28
  non-merges and ten PR merges (#453–#462). It contains runtime routing,
  CI/release hardening, deferred live-store vacuum, compression replay identity,
  managed-skill ownership, bounded runtime teardown, and safe-upgrade messaging.
  Merge PRs #453 through #462 carry these changes; release-only commits and
  merges change `CHANGELOG.md`, `Cargo.toml`, and `Cargo.lock` without an
  independent runtime contract.
- **[FACT] Design history (`M..D`)** is 50 commits: 42 non-merges and eight
  merges. All six `origin/master` reconciliation merges (`2208969d`, `9561cc30`,
  `0dc1ee14`, `363de8c7`, `859e5ed3`, `f1e2ec32`) are in this range, alongside
  foundation merges `2f029bbb` and `e3199ed3`. It adds and repeatedly reconciles
  the canonical master plan, complete numbered V2 plan set, architecture
  authority and generator, generated views, corpus fixtures, and writer bounds.
- **[INFERENCE — proposed disposition]** Preserve surviving runtime behavior and
  tests at `D`; exclude release-only version churn as independently normative;
  supersede intermediate plan drafts with the surviving files at `D`; retain
  merge and draft commits as provenance; require explicit owner decisions for
  every contradiction in §8 before calling this a refreshed accepted baseline.

## 5. Complete changed-file inventory and classification

Every one of the 125 endpoint-delta paths appears below. A grouped classification
is deliberate: files are individually enumerated, and each group has one stated
normative rationale.

### 5.1 Normative runtime source (20)

**[FACT]** These files change executable behavior or shipped Hermes guidance and
must be preserved or explicitly superseded:

- `src/agents/hermes/templates.rs`
- `src/agents/hermes/templates/plugin_init.py`
- `src/agents/hermes/templates/skill.md`
- `src/agents/mod.rs`
- `src/analytics_bridge.rs`
- `src/automation/backend.rs`
- `src/automation/memory_curator.rs`
- `src/automation/skill_materialization.rs`
- `src/automation/skill_writer.rs`
- `src/automation/skill_writer/consolidation.rs`
- `src/cli.rs`
- `src/global_db.rs`
- `src/hooks/steering.rs`
- `src/main.rs`
- `src/mcp/tools/handlers/session.rs`
- `src/memory/store.rs`
- `src/sessions/hermes.rs`
- `src/sessions/lcm/compression.rs`
- `src/sessions/mod.rs`
- `src/update_cmd.rs`

### 5.2 Normative tests and machine contracts (19)

**[CONTRACT]** These encode expected behavior. They are normative evidence even
where this synthesis did not execute them:

- `tests/agent_suite/agent_test.rs`
- `tests/agent_suite/cli_args_contract_test.rs`
- `tests/agent_suite/plugin_skill_contract_test.rs`
- `tests/agent_suite/skill_materialization_test.rs`
- `tests/agent_suite/skill_targets_test.rs`
- `tests/architecture_boundaries.rs`
- `tests/automation_runner_test/backend.rs`
- `tests/automation_runner_test/skill_writer.rs`
- `tests/core_cli_suite/cli_non_interactive_test.rs`
- `tests/dashboard_api_test/automation_skills.rs`
- `tests/dogfood_command_test.sh`
- `tests/hermes_suite/lcm_bridge.rs`
- `tests/mcp_suite/mcp_handler_test.rs`
- `tests/memory_suite/memory_test.rs`
- `tests/release_workflow_contract_test.sh`
- `tests/session_suite/lcm_compression.rs`
- `tests/session_suite/structured_backfill.rs`
- `tests/storage_suite/profile_storage_migration_test.rs`
- `tests/transcript_ingest_suite/hermes.rs`

### 5.3 Normative V2 product plans (29)

**[CONTRACT]** All plans `00` through `28` survive at `D` and define product
intent across domain, store, capture, projectors, query, policy, hooks, tools,
application, API, dashboard, migration, provenance, regression, retrieval,
scope, SDKs, privacy, convergence, configuration, CLI/MCP, context, LCM,
execution, indexing, observability, bundles, and remote operation. None may be
blanket-classified as out of scope:

- `docs/plans/tracedecay-v2/00-plan-set-index.md`
- `docs/plans/tracedecay-v2/01-domain-crate.md`
- `docs/plans/tracedecay-v2/02-store-crate.md`
- `docs/plans/tracedecay-v2/03-capture-crate.md`
- `docs/plans/tracedecay-v2/04-projectors-crate.md`
- `docs/plans/tracedecay-v2/05-query-crate.md`
- `docs/plans/tracedecay-v2/06-policy-crate.md`
- `docs/plans/tracedecay-v2/07-hooks-crate.md`
- `docs/plans/tracedecay-v2/08-tool-catalog-crate.md`
- `docs/plans/tracedecay-v2/09-application-crate.md`
- `docs/plans/tracedecay-v2/10-api-crate.md`
- `docs/plans/tracedecay-v2/11-dashboard-frontend.md`
- `docs/plans/tracedecay-v2/12-root-compatibility-migration.md`
- `docs/plans/tracedecay-v2/13-research-provenance-and-context-anchors.md`
- `docs/plans/tracedecay-v2/14-historical-failure-regression-matrix.md`
- `docs/plans/tracedecay-v2/15-search-quality-evaluation-and-retrieval-research.md`
- `docs/plans/tracedecay-v2/16-cross-project-repository-worktree-scope.md`
- `docs/plans/tracedecay-v2/17-official-public-api-and-sdks.md`
- `docs/plans/tracedecay-v2/18-secret-detection-redaction-and-private-data-safety.md`
- `docs/plans/tracedecay-v2/19-system-defragmentation-convergence-and-extensibility.md`
- `docs/plans/tracedecay-v2/20-configuration-control-plane.md`
- `docs/plans/tracedecay-v2/21-cli-mcp-tool-surface-and-output-unification.md`
- `docs/plans/tracedecay-v2/22-incremental-context-scout-and-suggestion-envelopes.md`
- `docs/plans/tracedecay-v2/23-session-lcm-temporal-retrieval-and-evaluation.md`
- `docs/plans/tracedecay-v2/24-canonical-task-plan-graph-and-multi-agent-executor.md`
- `docs/plans/tracedecay-v2/25-code-intelligence-indexing-crate.md`
- `docs/plans/tracedecay-v2/26-observability-accounting-and-usage.md`
- `docs/plans/tracedecay-v2/27-cross-host-agent-plugin-bundles.md`
- `docs/plans/tracedecay-v2/28-remote-multi-machine-shared-brain.md`

### 5.4 Normative architecture and governance (13)

**[CONTRACT]** The architecture prose is normative intent; generated views are
reviewable projections; the TOMLs plus `tests/architecture_boundaries.rs` are
machine-enforced governance:

- `architecture-boundaries.toml`
- `architecture-dependency-policy.toml`
- `docs/architecture/v2/dashboard-and-renderers.md`
- `docs/architecture/v2/frontend-build-and-embedding.md`
- `docs/architecture/v2/generated/convergence-scorecard.md`
- `docs/architecture/v2/generated/dependency-dag.md`
- `docs/architecture/v2/generated/owners.md`
- `docs/architecture/v2/generated/release-policy.md`
- `docs/architecture/v2/identity-and-evidence.md`
- `docs/architecture/v2/logical-brain.md`
- `docs/architecture/v2/privacy-and-retention.md`
- `docs/architecture/v2/query-and-api.md`
- `docs/architecture/v2/storage-and-consistency.md`

### 5.5 Corpus fixtures and corpus validation (19)

**[CONTRACT]** These are non-production but normative provider/identity corpus
evidence; they do not prove current host-hook event mappings:

- `tests/fixtures/v2/manifest.json`
- `tests/fixtures/v2/providers/antigravity.json`
- `tests/fixtures/v2/providers/claude.json`
- `tests/fixtures/v2/providers/cline.json`
- `tests/fixtures/v2/providers/codex.json`
- `tests/fixtures/v2/providers/copilot.json`
- `tests/fixtures/v2/providers/cursor.json`
- `tests/fixtures/v2/providers/gemini.json`
- `tests/fixtures/v2/providers/hermes.json`
- `tests/fixtures/v2/providers/kilo.json`
- `tests/fixtures/v2/providers/kimi.json`
- `tests/fixtures/v2/providers/kiro.json`
- `tests/fixtures/v2/providers/opencode.json`
- `tests/fixtures/v2/providers/roo-code.json`
- `tests/fixtures/v2/providers/vibe.json`
- `tests/fixtures/v2/providers/zed.json`
- `tests/v2_corpus_suite/corpus_test.rs`
- `tests/v2_corpus_suite/generate_10x.py`
- `tests/v2_corpus_suite/main.rs`

### 5.6 Tooling, CI, and agent scaffolding (18)

**[CONTRACT]** These do not independently define runtime semantics, but are
normative for build/release reproducibility, dogfood operation, plan execution,
architecture generation, and shipped agent instructions:

- `.cargo/config.toml`
- `.config/nextest.toml`
- `.github/workflows/ci.yml`
- `.github/workflows/release-beta.yml`
- `.github/workflows/release-plz.yml`
- `.github/workflows/release-pr-integrity.yml`
- `.github/workflows/release.yml`
- `.claude/skills/dogfooding-tracedecay/SKILL.md`
- `.codex/skills/dogfooding-tracedecay/SKILL.md`
- `.codex/skills/dogfooding-tracedecay/agents/openai.yaml`
- `.codex/skills/executing-tracedecay-v2-plan/SKILL.md`
- `.codex/skills/executing-tracedecay-v2-plan/agents/openai.yaml`
- `.codex/skills/executing-tracedecay-v2-plan/scripts/plan_execution.py`
- `.codex/skills/executing-tracedecay-v2-plan/scripts/plan_inventory.py`
- `.codex/skills/executing-tracedecay-v2-plan/scripts/test_plan_execution.py`
- `.codex/skills/executing-tracedecay-v2-plan/scripts/test_plan_inventory.py`
- `scripts/generate_architecture_views.py`
- `scripts/hermes_plugin_unit_check.py`

### 5.7 Non-normative or contextual endpoint files (7)

**[FACT / proposed classification]** These are explicitly non-normative as
independent product contracts, with the stated exceptions:

- `.changeset/runtime-routing-ci-hardening.md`, `CHANGELOG.md`, `Cargo.toml`, and
  `Cargo.lock`: release/package evidence; behavior is normative only through the
  corresponding source and tests, not the version bump itself.
- `scripts/dogfood.sh` and `scripts/hermes_stock_integration.sh`: developer and
  integration harnesses; operationally important but not product semantics.
- `docs/plans/2026-07-09-tracedecay-brain-rewrite.md`: canonical product,
  global-gate, PR-slice, and dependency-order authority. Numbered owner plans own
  bounded implementation detail; this audit is evidence-only and overrides
  neither.

Counts: 20 + 19 + 29 + 13 + 19 + 18 + 7 = **125**.

## 6. Findings by subsystem

### 6.1 Runtime routing and Hermes bridge

- **[FACT]** `STANDARD_HERMES_LCM_PROVIDER` changes from `cursor` to `hermes` in
  `src/agents/hermes/templates/plugin_init.py`; expectations change in
  `tests/hermes_suite/lcm_bridge.rs` (`2d06a4d1`, `89f416b1`).
- **[FACT]** project resolution changes to registry/alias-aware
  `tracedecay_project_context`, and Hermes-home rejection changes from equality
  to prefix containment (`8bdbce98`; `templates.rs`, `plugin_init.py`).
- **[FACT]** one turn is projected sequentially to the user store and every
  touched project root (`for project_root in [None, *project_roots]`), with
  `associated_project_roots` preserved through the MCP live projection
  (`3863b45a`; `plugin_init.py`, `session.rs`). Per-root failure is fail-open.
- **[FACT]** `src/sessions/hermes.rs` bumps `user-turn-v1` to `user-turn-v2` and
  removes registered-project exclusion from the canonical user sweep.
- **[FACT]** analytics now counts project session shards and all-project fan-out
  (`src/analytics_bridge.rs`, `src/sessions/mod.rs`, `src/global_db.rs`).
- **[INFERENCE / disposition]** Carry this model, but require per-shard receipts
  and reconciliation. Sequential best-effort writes do not prove eventual
  convergence after a mid-loop shard failure.

### 6.2 Compression and retry behavior

- **[FACT]** replayed messages with non-empty `lcm_summary_node_id` bypass raw
  re-ingest; identity precedence is explicit ID, store-ID lookup, deterministic
  fallback (`src/sessions/lcm/compression.rs`, `2d06a4d1`).
- **[CONTRACT]** `idless_compression_replay_does_not_reingest_existing_raw_messages`
  pins no-duplication behavior.
- **[FACT]** `d50588d7` moves memory-curator messages out of duplicated prompt
  text into backend context. `classify_agent_task_error_message` classifies an
  immediate oversized request as `Permanent`; `AGENT_TASK_MAX_ATTEMPTS = 3`
  plus the job time budget bounds in-run retry. `agent_task_failure_disposition`
  treats only a stale recorded oversize ledger as `Retryable`, allowing a later
  rebuilt request after request-bounding changes.
- **[INFERENCE / disposition]** Preserve replay identity and the existing
  bounded automation behavior. There is no new oversize-retry owner obligation.

### 6.3 Live memory and storage maintenance

- **[FACT]** `MemoryStore::remove_fact` no longer calls inline incremental vacuum;
  the deleted helper has no surviving reference in `src/memory/store.rs`
  (`1d42aa77`). Freed pages remain until exclusive maintenance.
- **[CONTRACT]** `remove_fact_defers_vacuum_while_peer_connections_are_live`
  expects a non-empty freelist, usable peers, and no immediate shrink.
- **[FACT]** update flow prints quiesce/maintenance receipts, but the messaging
  adds no interrupt guard (`src/update_cmd.rs`, `e008fd94`).
- **[CONTRACT]** `f18f0f14` changes all five store entries to
  `physical_writer = "store"` plus semantic producers, enforced by
  `tests/architecture_boundaries.rs`.
- **[INFERENCE / disposition]** Carry deferred reclamation and daemon-owned
  writers; add a periodic exclusive-maintenance cadence independent of upgrades.

### 6.4 Shutdown and cleanup

- **[FACT]** `246427a6` added a timeout around daemon background-task shutdown;
  `42d7ce8e` fully reverted it and added `runtime.shutdown_timeout(2s)` in
  `src/main.rs`. `src/daemon.rs` is net-zero across `B..D`.
- **[FACT]** internal scheduler shutdown has bounded components, and watcher/task
  paths abort and join, but endpoint `shutdown_background_tasks().await` has no
  outer deadline.
- **[INFERENCE / resolved disagreement]** The runtime-drop bound can limit Tokio
  blocking-pool teardown only after `run(cli)` returns. It cannot rescue an async
  future that prevents `run` from returning. A refreshed baseline must not claim
  deterministic bounded process shutdown without an end-to-end test.

### 6.5 Managed-skill ownership

- **[FACT]** automation update and consolidation now reject any source other than
  `ManagedSkillSource::AutomationRun`; `UserDraft` and `Import` are protected
  (`31d3aee1`; `skill_writer.rs`, `consolidation.rs`).
- **[FACT]** exports/materialization skip non-default profile roots through
  `uses_default_user_profile`, affecting CLI, dashboard, lifecycle, and
  auto-enable paths (`src/agents/mod.rs`, `skill_materialization.rs`,
  `skill_writer.rs`).
- **[FACT]** the predicate compares a profile root with
  `home.join(".tracedecay")`; the configuration path can canonicalize an existing
  symlinked parent. No changed test covers a symlinked home.
- **[INFERENCE / disposition]** Preserve ownership protection, but canonicalize
  both sides (or compare stable identities) and return an explicit skip reason.
  Otherwise a symlinked home or intentionally relocated production profile can
  silently disable host exports.

### 6.6 Host hooks and steering

- **[FACT]** Host-hook registration, command dispatch, event mapping, error
  handling, and uninstall files are byte-identical `B..D`. The only changed
  `src/hooks/**` file is `src/hooks/steering.rs`; Hermes Python callbacks are a
  provider bridge, not OS hooks.
- **[FACT]** Codex steering removes verbose delegation/ownership prose, then
  restores the eight agent names in compact form (`42389dc6`, `a67c1ec3`).
- **[FACT]** current asymmetries are unchanged: Codex registers six hooks through
  `config.toml` and maintains a trust table; Claude bundle JSON has seven entries
  and uniquely registers `PostToolUseFailure`; Codex has no current PreToolUse
  guard; Hermes uses in-process callbacks.
- **[CONTRACT vs FACT]** plan 07 targets larger event sets and says the V2 bundle
  must not edit host trust state, while current Codex installation writes hook
  trust entries. This is a design target/current implementation contradiction,
  not an in-range regression.
- **[INFERENCE / disposition]** Restore a compact parent-owns-writes token for
  Codex, document host asymmetries, and reconcile trust ownership before V2 hook
  implementation begins.

## 7. Compatibility, migration risks, and expected failure modes

1. **[INFERENCE — high] Silent managed-skill export skip.** Symlink/canonical path
   mismatch or relocated `TRACEDECAY_DATA_DIR` can produce a successful no-op.
2. **[INFERENCE — medium] Partial multi-shard projection.** User store can advance
   while a project shard fails; no per-shard durable reconciliation is shown.
3. **[INFERENCE — medium] Unbounded async shutdown.** A never-resolving outer
   background shutdown await can prevent the runtime timeout call from executing.
4. **[INFERENCE — medium] Maintenance-dependent growth.** High fact churn can
   accumulate freelist pages indefinitely between exclusive windows.
5. **[INFERENCE — low/expected] Full user resweep.** `user-turn-v2` starts from a
   new cursor namespace, increasing upgrade CPU/IO/storage and leaving v1 cursor
   rows orphaned unless explicitly cleaned.
6. **[INFERENCE — low] Provider discontinuity.** Historical Hermes records labeled
   `cursor` are not remapped to `hermes`; provider-filtered analytics split.
7. **[INFERENCE — low-to-medium] Notification multiplication.** A turn emits two
   event types, each once per user/project destination: up to
   `2 × (1 + unique_project_roots)` total.
9. **[INFERENCE — low] Codex ownership guidance loss.** Compact steering preserves
   agent discovery but drops the only Codex-specific parent-owns-writes sentence.
10. **[INFERENCE — low] Dogfood packaging.** `CARGO_MANIFEST_DIR`-relative scripts
    may not exist beside distributed binaries; failure should remain explicit.

## 8. Omissions or contradictions that can invalidate a refreshed baseline

> Do not declare the baseline refreshed while any item below is silently assumed
> resolved.

1. **Shutdown contract overclaim:** deterministic shutdown/checkpoint prose versus
   an unbounded outer async await.
2. **Missing multi-shard reconciliation:** “searchable from every touched repo” is
   not guaranteed after partial fan-out failure.
3. **Missing periodic reclamation policy:** deferred vacuum is safe for peers but
   has no demonstrated independent cadence.
4. **Canonical-path contradiction:** default-profile detection compares paths with
   different canonicalization semantics and silently skips.
5. **Hook trust contradiction:** plan 07 says bundles do not edit trust state;
   current Codex installer does.
6. **Hook target/current mismatch:** target event counts and asymmetries must be
   explicit migration work, not treated as already implemented.
7. **Provider migration omitted:** no `cursor` to `hermes` continuity/backfill.
8. **Cursor migration omitted:** no explicit resweep budget, resumability proof,
   or v1 cursor cleanup.
9. **Notification cardinality unspecified:** two event types each deliver
   `1 + unique_project_roots`; dedupe and partial-failure behavior are unbounded.
11. **Test-execution limitation:** source-inspected integration contracts are not a
    passing suite. Aggregate verification is required before acceptance.
12. **Endpoint drift:** any later design/master commit is outside this report until
    its SHA and changed-path inventory are re-audited.

## 9. Owner-plan impacts and proposed normative dispositions

- **Store / plans 02, 19:** preserve daemon-owned physical writers and deferred
  live vacuum; add explicit maintenance scheduling and receipts.
- **Projectors / plans 04, 16, 23:** preserve user canonical plus project
  projections; add per-shard idempotent receipts and catch-up reconciliation.
- **Migration / plan 12:** specify user-turn-v2 resweep, provider continuity,
  orphan-cursor policy, interruption/resumption, and storage budget.
- **Hooks / plans 07, 27:** preserve verified current asymmetries until deliberately
  migrated; decide trust-state owner and compact Codex write ownership language.
- **Root lifecycle / plan 12 (application plan 09 at the `run(cli)` surface):**
  bind global daemon/process shutdown to an end-to-end deadline. Plan 24 owns
  executor attempt deadlines, not global daemon shutdown. No new oversize retry
  obligation is introduced.
- **Observability / plan 26:** distinguish skipped export, partial projection,
  runtime-drop timeout, async shutdown timeout, and both notification event
  types. Retired FM-168 adds no retry-exhaustion obligation.
- **Architecture / storage-and-consistency:** do not mark deterministic shutdown
  satisfied until a never-resolving-task process test passes.

## 10. Required regression scenarios

1. Symlinked `$HOME` and relocated production data root still export managed
   skills, or return an explicit intentional-skip receipt.
2. A project-shard failure after user-store success is later reconciled with the
   same message ID and no duplicate.
3. A never-resolving background task cannot prevent process exit beyond the
   declared shutdown deadline.
4. User-turn-v1 data is fully and idempotently reswept into v2, including an
   interrupted/resumed run and explicit old-cursor disposition.
5. Provider continuity across `cursor` and `hermes` is either migrated or
   intentionally queryable as two eras.
6. Repeated fact deletion leaves peers usable; scheduled exclusive maintenance
   later reclaims pages within a declared bound.
7. Existing automation tests pin immediate oversize as `Permanent`, stale-ledger
   healing as later-run `Retryable`, and the three-attempt/total-time budget.
8. Both Hermes notification event types each emit exactly
   `1 + unique_project_roots`, with dedupe and partial-failure behavior pinned.
9. Codex steering retains all eight agents and a compact parent-owns-writes token.
10. Contract test pins Claude-only `PostToolUseFailure`, absent Codex PreToolUse,
    Codex config registration/uninstall, and the chosen trust-state owner.
11. Architecture generation reproduces owners, DAG, release policy, and
    convergence scorecard without drift.
12. Run the focused tests named above, then `cargo nextest run --workspace
    --no-fail-fast` with isolated `TRACEDECAY_DATA_DIR` before owner acceptance.

## 11. Acceptance statement

**[FACT]** The endpoint delta has 125 paths and §5 enumerates/classifies 125.
**[FACT]** Runtime routing, compression, live memory, storage, shutdown,
managed-skill ownership, Hermes/Codex/Claude hook behavior, migration,
compatibility, failure modes, plans, architecture, CI/tooling, and corpus evidence
are covered. **[INFERENCE]** The proposed dispositions are review inputs, not
accepted product decisions. The omissions in §8 are intentionally prominent;
silently dropping any of them would invalidate this refreshed baseline.
