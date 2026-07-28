# Agent Notes

TraceDecay is a code-intelligence tool (Rust workspace + TypeScript dashboard)
that builds a semantic knowledge graph from many languages and serves it to
agent hosts through MCP, hooks, LSP, and an embedded dashboard.

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

## Learned User Preferences

- In shared checkouts, honor active file ownership: re-read before editing,
  commit only self-consistent owned paths, and never sweep in peer work.
- Before launching Cargo, check for an equivalent active run; reuse or wait,
  batch narrow local checks, prefer CI for aggregate verification when the
  shared target is contended, and never kill peer build processes.
- Require measured, falsifiable verification and root-cause fixes; preserve
  byte-exact identity contracts, and never weaken assertions, raise timeouts,
  ignore tests, or mask gate failures.
- Keep every user-facing state truthful: unavailable, unsupported, partial,
  or failed data must never render as successful zero, empty, or complete.
- Verify cutovers through real production callers and every exposed surface;
  compiled, unit-tested, or source-mentioned code alone is insufficient.
- Audit all supported host integrations when changing shared host behavior;
  do not treat one host as representative of the complete integration set.
- Parallelize independent work aggressively, but inspect active agents first,
  keep file ownership disjoint, and coordinate shared integration centrally.
- Commit coherent completed fixes incrementally with explanatory conventional
  messages instead of holding a large mixed working tree.
- Keep `cargo dogfood` as the fast development-build path; do not add a
  separate release-dogfood mode for local iteration.
- Resolve conflicts and integrate parallel work from relevant transcripts,
  plans, and Git history so intent—not whichever side is newer—wins.

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
- Dogfood targets the real managed profile; back up live databases before
  mutation and never run a second daemon against that profile.
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
