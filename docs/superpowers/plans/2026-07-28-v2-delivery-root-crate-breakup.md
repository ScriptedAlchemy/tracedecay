# V2 Delivery and Root-Crate Breakup Implementation Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../plans/tracedecay-v2/NEXT.md), and the applicable numbered V2
> plan. Do not recreate its task checklists, file inventories, branch/worktree/SHA
> or commit protocol, Gate A/B, timing/JUnit receipts, exact test names/counts,
> generated-byte/source-shape checks, PR closure gates, or platform gate lattice.
> Historical version, alias, deprecation, or migration language applies only
> where `origin/master`, a published package/release, or live persistence
> proves a predecessor; otherwise the current contract changes in place.

**Outcome contributed:** This plan recorded a leaves-first root-crate breakup,
measured iteration-cost experiments, and direct, truthful product-journey goals
for the work later organized as PR14–PR20.

**Historical design summary:** Query and code index moved first, followed by pure
projectors, capture/application ports, the MCP-daemon boundary, API routes, and
adapter/runtime ownership.

**Tech stack:** Rust 2024 workspace, Cargo/nextest, rusqlite, Axum/MCP/LSP,
React/TypeScript/Rsbuild/Vitest/Playwright, generated schemars contracts.

## Historical execution constraints (non-authoritative)

- The integration branch is `codex/tracedecay-total-redesign-plan`; preserve
  its conventional commit history and never squash or restack peer commits.
- Serialize commits, broad verification, push, and worktree creation. Before
  every broad Cargo command, wait for an equivalent or target-contending Cargo
  process; never kill a peer build.
- Audit staged and unstaged state immediately before each commit. Use an
  isolated index per owner/purpose; never stash, reset, clean, or `git add -A`.
- Push only after the Phase 0 gate is honestly green. Branch new work from the
  exact pushed `BASE_SHA`, never from unpublished local history.
- Use ordinary repository-local Cargo targets. Build dashboard assets from the
  worktree's own sources before its first Rust check. Do not run a second daemon
  or use `cargo dogfood` during extraction.
- Every user-facing partial, unavailable, denied, stale, unsupported, or
  unmeasured state remains explicit; no surface converts it to success or zero.

## Product outcomes contributed

- Leaves-first extraction reduced root-crate coupling while preserving stable
  compatibility entry points.
- Work/runtime delivery separated core task authority from later advanced
  workflow, placement, automation, and host-handoff behavior.
- Dashboard work emphasized truthful state, responsive keyboard operation,
  accessibility, and desktop visual evidence.
- Draft PR #421 was the historical consolidated delivery vehicle.

## Historical child-plan registry

Extraction sequence:

1. [`v2/01-domain-request-context.md`](v2/01-domain-request-context.md)
2. [`v2/02-query-extraction.md`](v2/02-query-extraction.md)
3. [`v2/03-code-index-extraction.md`](v2/03-code-index-extraction.md)
4. [`v2/04a-projectors.md`](v2/04a-projectors.md)
5. [`v2/04b-capture-ports.md`](v2/04b-capture-ports.md)
6. [`v2/04c-application-orchestration.md`](v2/04c-application-orchestration.md)
7. [`v2/05a-mcp-daemon-cycle-break.md`](v2/05a-mcp-daemon-cycle-break.md)
8. [`v2/05b-api-routes.md`](v2/05b-api-routes.md)
9. [`v2/05c-adapter-runtime-pr19.md`](v2/05c-adapter-runtime-pr19.md)

Product sequence:

1. [`v2/pr14-work-dashboard.md`](v2/pr14-work-dashboard.md)
2. [`v2/pr15-multi-root.md`](v2/pr15-multi-root.md)
3. [`v2/pr16-remote-brain.md`](v2/pr16-remote-brain.md)
4. [`v2/pr17-residual-workflow.md`](v2/pr17-residual-workflow.md)
5. [`v2/pr18-public-sdks.md`](v2/pr18-public-sdks.md)
6. [`v2/pr19-cutover-runtime.md`](v2/pr19-cutover-runtime.md)
7. [`v2/pr20-performance.md`](v2/pr20-performance.md)

## Historical Phase 0: publish a reproducible baseline

- [ ] Inventory `origin/codex/tracedecay-total-redesign-plan..HEAD`, commit
      owners, staged/unstaged/untracked paths, active worktrees, and Cargo jobs.
- [ ] Commit each remaining coherent owner/purpose group with a conventional
      subject through an isolated temporary index.
- [ ] Run, serially:
      `cargo check -p tracedecay --lib --all-features`;
      `cargo check -p tracedecay-domain --lib --all-features`;
      `cargo nextest run --workspace --all-features --profile ci --locked`;
      exact default-lifecycle, lite-grammar, and test-transport-crash journeys;
      dashboard type/boundary/contracts/Vitest;
      `dashboard_api_test`; and workspace package verification.
- [ ] Retain `target/nextest/ci/junit.xml` and report exact total/pass/fail,
      every command, wall time, skip, and failure.
- [ ] Push the integration branch and record the remote-confirmed `BASE_SHA`.
- [ ] At `BASE_SHA`, record TraceDecay health and Plan 33 no-op/query/index/
      MCP/focused-test/root-leaf/domain-leaf timing receipts with host,
      toolchain, feature set, warmth, and rebuilt units.

## Historical Phase 0b: bootstrap exact-SHA worktree

- [ ] Fetch `origin`, verify `.worktrees` is ignored, and ensure neither
      `.worktrees/v2-root-breakup` nor `codex/v2-root-breakup` already exists.
- [ ] Create `/fast/projects/tracedecay/.worktrees/v2-root-breakup` on
      `codex/v2-root-breakup` from the exact pushed `BASE_SHA`.
- [ ] From that worktree's `dashboard/`, run `npm ci` and `npm run build`;
      verify `app-dist` and its source stamp were produced from that checkout.
- [ ] After checking for active Cargo jobs, run
      `cargo check -p tracedecay --lib --all-features`.
- [ ] Report path, branch, HEAD, dashboard proof, exact command timing, and
      cleanliness. Leave the shared integration checkout untouched at handoff.

## Historical Gate A: query and code-index extraction

- Query and code-index private leaf edits each improve at least 20% or 8s on
  identical warm same-host checks; otherwise record `pending`/`fail` and revise
  or revert the boundary.
- Root no longer compiles moved sources inline; focused tests do not relink the
  full root; default/all/lite/package and extraction/index digest contracts are
  equivalent.
- WGSL and grammar build ownership no longer reruns for unrelated root edits.

## Historical Gate B: cycle-break evidence before PR14 implementation

- Root all-feature leaf touch improves at least 10% or 12s versus 121.35s, or
  records a non-blocking `pending` disposition naming dominant rebuilt units.
- MCP-handler edit improves at least 15% or the dependency-direction test lands.
- Default/all/no-default/lite/package/platform behavior remains equivalent,
  with no cycle, hidden production gap, duplicate façade, or widened visibility.

## Historical per-slice protocol

For every child-plan slice:

1. Pin baseline commit, health/status, impacted callers, and affected tests.
2. Record identical warm command, edit class, wall time, and rebuilt units.
3. Write a failing direct or negative test before behavior changes.
4. Move whole modules natively; use graph-safe symbol edits for callables.
5. Wire manifests, features, build ownership, package includes, façades, and
   generated contracts explicitly.
6. Run package checks/tests, architecture contracts, and affected journeys.
7. Measure the treatment on the same host and retain only valid evidence.
8. Commit one compile-green/test-green logical slice; rollback is `git revert`.

## Current acceptance authority

Current direct product behavior and acceptance are defined by the plan-set
index, `NEXT.md`, and the applicable numbered V2 plans linked above. This
historical plan contributes no independent PR closure or platform gate.
