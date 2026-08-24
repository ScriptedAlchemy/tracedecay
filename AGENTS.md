# Agent Notes

## Cargo

- Do not commit an absolute `[build].target-dir`; hosted CI and published packages must use repo-local or runner-local paths.
- **Default: build into the checkout's own repo-local `target/` directory** (each worktree
  has its own; checkouts under `/fast/projects/` are already on the fast disk, so this is
  both isolated and fast). No `CARGO_TARGET_DIR` override needed in the normal case.
- **If the repo-local target dir is locked/contended** (another process holds the cargo
  build lock — "Blocking waiting for file lock on build directory" — or a concurrent agent
  owns the checkout), fall back to a cache target dir on the fast volume:
  `CARGO_TARGET_DIR=/fast/cargo-target/<repo-or-worktree-name>` (e.g.
  `/fast/cargo-target/tracedecay-merge-check`). Never place target dirs under `/tmp`,
  `$HOME`, or anywhere on the root disk.
- Cargo-launched TraceDecay test data follows the active target dir:
  repo-local default → `TRACEDECAY_DATA_DIR=target/test-profile/.tracedecay`; fast-cache
  fallback → `TRACEDECAY_DATA_DIR=<CARGO_TARGET_DIR>/test-profile/.tracedecay`.
- Run normal repo commands from the repo root: `cargo check`, `cargo test`, `cargo test-all`, `cargo nextest run --workspace --no-fail-fast`.
- Toolchain caches (`sccache`, cargo registry) live under `/fast/cache/` and need no
  per-agent changes.
- CI is unchanged and keeps runner-local paths:

```sh
CARGO_TARGET_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-cargo-target" \
TRACEDECAY_DATA_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-test-profile/.tracedecay" \
cargo test-all
```

## Learned User Preferences

- Do not merge a batch of PRs until aggregate verification is stable; a single flaky pass is not enough.
- Delegate code edits to execution-focused subagents; use planning/review-focused agents for planning, review, and thinking.
- When orchestrating parallel agents, the lead dictates exact scoped edits, subagents execute, and the lead reviews diffs before any push.
- Subagents should not invent scope beyond what the lead dictated.

## Git

- Every non-merge commit subject must pass
  `npm run lint:commit -- --from origin/master --to HEAD` before push.
- Use `<type>: <subject>` or `<type>(<scope>): <subject>` with one of:
  `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`.
- Keep the subject at 72 characters or fewer. Example: `fix(doctor): avoid false orphan warnings`.

## Learned Workspace Facts

- Parallel branch work uses git worktrees under `.worktrees/` in the repo root (for example `.worktrees/codex-cli-args-stdin`).
- Integration/default branch is `master` (GitHub: ScriptedAlchemy/tracedecay).
- Multi-PR merge verification: build a detached temporary worktree on `origin/master`, merge all target branches, then run tests with isolated `CARGO_TARGET_DIR` (under `/fast/cargo-target/`, see the Cargo section) and `TRACEDECAY_DATA_DIR` paths.
