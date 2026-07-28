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
