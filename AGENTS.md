# Agent Notes

TraceDecay is a code-intelligence tool (Rust workspace + TypeScript dashboard)
that builds a semantic knowledge graph from many languages and serves it to
agent hosts through MCP, hooks, LSP, and an embedded dashboard.

## Overall Objective

Deliver a fully integrated final-V2 product through real production journeys,
truthful typed states, maintainable crate/module boundaries, and direct
behavioral evidence—not PR choreography, gate scaffolding, or code that merely
compiles.

## Layout

- `src/` — main `tracedecay` crate (daemon, MCP tools, global DB, sessions,
  code index, application services).
- `crates/` — workspace member crates (`tracedecay-api`, `-application`,
  `-domain`, `-store`, `-hooks`, `-policy`, `-tool-catalog`, rusqlite
  parity/runtime crates).
- `dashboard/` — the single embedded dashboard (React + rsbuild + vitest).
  `dashboard/src/contracts/` is generated from Rust schemas via schemars —
  never hand-edit it; regenerate with the `contracts:generate` script and
  verify with `contracts:check`.
- `plugin/` — host bundles (Claude, Codex, Cursor, Kimi, opencode).
- `tests/` — integration test suites; `benches/` — criterion benches;
  `eval/`/`evals/` — hermetic and adoption evals; `docs/` — plans and guides.
- `scripts/` — CI/dev gates (commit-msg check, bundle checks, release drift).

## Build & test

- Ordinary `cargo` commands; edition 2024, resolver 3. Scope checks to the
  smallest touched package/target during development.
- Before handoff, run a broader gate from the repo root, e.g.
  `cargo check --all-features` or
  `cargo nextest run --workspace --all-features --no-fail-fast`.
- Dashboard: `npm run build` (rsbuild), `npm run typecheck` (`tsc --noEmit`),
  `npm test` (vitest) from `dashboard/`.
- libtest `--exact` requires the full module path and exits 0 when a filter
  matches nothing — a vacuous "0 passed" green. For name-filtered runs prefer
  the ad-hoc anti-vacuity helper `scripts/require-exact-test.sh`; it is not a
  reason to ossify CI or test names. Otherwise pass the full path
  (`module::path::test_name`) and confirm the reported count is non-zero before
  treating a run as evidence.
- `dashboard/app-dist/` is gitignored build output but required by `build.rs`;
  a fresh checkout/worktree must build the dashboard (or seed the directory)
  before Rust compiles. `TRACEDECAY_SKIP_DASHBOARD_BUILD=1` only skips a
  stale rebuild.

## Conventions

- Commits: `<type>(<scope>): <subject>` (subject ≤ 72 chars) with one of
  `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`,
  `style`, `test`. Every non-merge commit subject must pass
  `scripts/check-conventional-commits.sh`.
- Integration branch is `master` (GitHub: ScriptedAlchemy/tracedecay); CI
  lives in `.github/workflows` (hidden — search with `rg --hidden`).
- `.github/`, `.githooks/`, and nested `AGENTS.md` files may carry more
  specific guidance; the deeper file wins.
- Keep changes minimal and scoped; match surrounding style; run the checks
  that cover your change before calling it done.

## Engineering Hygiene

- Reuse canonical TraceDecay authorities and maintained libraries first.
  Custom parsers, cursors, caches, retries, transports, registries, schedulers,
  crypto/auth/policy stores, or filesystem durability layers require a concrete
  TraceDecay-specific boundary and must delete more complexity than they add;
  otherwise skip the machinery. Prefer existing workspace dependencies, add a
  maintained dependency only for clear deletion/complexity benefit, avoid
  overlapping libraries, and remove a dependency with its last production
  caller.
- Do not create parallel or shadow authorities, contract-only phases,
  test-only production ports, mountless features, or availability claims
  without a real production caller and user journey.
- Accept direct behavior, not bureaucracy. Do not use source-shape/string
  scans, exact-test-name inventories, PR-specific
  snapshots/receipts/manifests/attestations, synthetic provider lookalikes,
  giant Cartesian matrices, or hard-coded gate counts as acceptance. Preserve
  runtime receipts, migration journals, compare-and-swap, hosted release
  provenance, and `--no-tests=fail`.
- Complete cutovers in one delivery slice: migrate every caller and datum,
  then delete compatibility façades, duplicate routes, old flags, dead aliases,
  and superseded scaffolding.
- Add a V2/V3 contract, compatibility alias, deprecation path, or data
  migration only after proving the prior shape shipped on `origin/master`, in
  a published package, or in a live persisted format. Branch-local and
  unreleased contracts change in place; a `V1` suffix alone is not release
  evidence and does not justify compatibility scaffolding.
- Keep hand-written modules focused: no new hand-written source file over
  1,000 lines and do not grow an existing oversized file. When safely possible,
  touching one should extract a cohesive responsibility. Generated code and
  checked-in fixtures/data are exempt.
- Keep boundaries explicit: use top-level explicit imports/reexports, avoid
  wildcard parent-child cycles and inline imports, maintain one generated wire
  authority, and do not hand-write duplicate DTOs.
- Keep production failures typed: add no `unwrap`, `expect`, `panic`, silent
  fallback, fabricated timestamp/default, empty success, or swallowed error.
  Tests may use assertions and unwraps where appropriate.
- Do not stage dead code or fake readiness with `allow(dead_code)`,
  placeholders, unreachable enum variants, or feature flags for unfinished
  production behavior. Wire it now or omit it truthfully.
- Comments and docs explain invariants and why; remove narration, stale PR
  language, and superseded plan authority. `00-plan-set-index.md` is the sole
  roadmap precedence; `NEXT.md` records current outcomes only, while historical
  plans and benchmarks are archival.
- Name production modules, APIs, tests, scripts, and CI jobs for durable product
  capabilities—not PR numbers, milestones, phases, or temporary gates. Keep
  PR/milestone labels only in clearly archival plans and benchmark provenance.
- Tests must be falsifiable and cover failure, denial, staleness, isolation,
  cancellation, and rollback where relevant, without duplicating the same
  substrate across every host × OS combination.

## Learned User Preferences

- In shared checkouts, honor active file ownership: re-read before editing,
  commit only self-consistent owned paths, and never sweep in peer work.
- Before launching Cargo, check for an equivalent active run; reuse or wait,
  batch narrow local checks, prefer CI for aggregate verification when the
  shared target is contended, and never kill peer build processes.
- Require measured, falsifiable verification and root-cause fixes; preserve
  byte-exact identity contracts, and never weaken assertions, raise timeouts,
  ignore tests, or mask gate failures.
- Audit all supported host integrations when changing shared host behavior;
  do not treat one host as representative of the complete integration set.
- Parallelize independent work aggressively, but inspect active agents first,
  keep file ownership disjoint, and coordinate shared integration centrally.
- Commit coherent completed fixes incrementally with explanatory conventional
  messages instead of holding a large mixed working tree.
- Resolve conflicts and integrate parallel work from relevant transcripts,
  plans, and Git history so intent—not whichever side is newer—wins.
- Before final review, checkpoint the lane, merge the latest explicit clean-main
  floor, and compare patch IDs plus owned paths. Drop duplicate or superseded
  work instead of carrying parallel implementations; regenerate canonical
  outputs after the merge rather than hand-merging generated files.

## Learned Workspace Facts

- Durable facts are project-wide and must survive branch or worktree deletion;
  branch stores are not their authoritative home.
- Linked worktrees share the primary checkout's project/store identity while
  retaining exact worktree snapshot authority.
- Historical convergence, repair/rebuilds, semantic model acquisition, and
  indexing run as bounded background work after required fail-closed checks;
  they must not block admission or exact, lexical, graph, or ordinary retrieval.
- Test fixtures must use shared production identity and enrollment authorities,
  isolate home, profile, project, and session inputs, and never read or mutate
  the operator's real TraceDecay or agent-host data.
- Missing registries and unavailable authorities are typed states, not
  transport errors or successful empty results.
- LCM retrieval, including paginated summary sources, hydrates through canonical
  redaction/content authority from each message's owning store; raw rows and
  ranked candidate metadata are not authoritative backfill.
- Cross-project memory selectors open the selected registered project's durable
  store with exact project/profile/store identity; they never alias the active
  project's memory database.
- `workspaceOpen` follow-up reads use daemon-wide typed route authority;
  linked-worktree requests retain registered identity and never fall back to
  the active graph.
- Recovery may clear only the exact dirty marker adopted under its sync lease;
  compare-and-swap must preserve foreign or newer markers.
