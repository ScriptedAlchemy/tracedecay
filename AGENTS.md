# Agent Notes

## Local Cargo Development (Zack's Machine Only)

- This section is an agent/workspace convention for developing TraceDecay in Zack's local
  checkouts. It is not a TraceDecay product requirement, public contributor requirement,
  published Cargo configuration, or hosted-CI policy.
- Do not encode these machine-specific paths or cache choices in tracked product behavior,
  repository Cargo configuration, public documentation, or CI solely to satisfy this section.
- Portable repository Cargo changes are allowed when measurements justify them,
  including manifests, profiles, features, build settings, and build-script
  configuration. Preserve stock-Cargo contributor, CI, release, and published
  package behavior; never hard-code this machine's target paths or slot policy.
- Invoke ordinary `cargo` commands. Zack's machine-local cargo shim/cargo-slot
  transparently allocates non-blocking lanes for concurrent Rust operations; do
  not bypass it, add repository lane coordination, or serialize Cargo work to
  avoid contention.
- Do not pause, kill, or disable Rust Analyzer to improve build timings. Its
  Claude Code LSP-owned processes are outside repository build optimization.
- The shim's local policy uses the checkout's repo-local `target/` by default
  and allocates isolated targets under `/fast/cargo-target/` when a non-blocking
  lane needs one. Let the shim select the lane; agents do not set
  `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` to manage contention.
- Cargo-launched TraceDecay test data follows the target selected by the shim.
  Never redirect targets or test data under `/tmp`, `$HOME`, or the root disk.
- During development, scope checks and test compilation to the smallest touched
  package, target, and feature set. A test-name filter does not reduce which
  test binary Cargo compiles, so batch focused tests by target where practical.
- Before handoff, run the relevant broader all-feature gate from the repo root:
  `cargo check --all-features`, `cargo test --all-features`, `cargo test-all`,
  or `cargo nextest run --workspace --all-features --no-fail-fast`.
- Toolchain caches (`sccache`, cargo registry) live under `/fast/cache/` and need no
  per-agent changes.
- Hosted CI and other developers follow their own environment/repository defaults; never
  assume this machine's `/fast` layout exists elsewhere.

## Learned User Preferences

- Do not merge a batch of PRs until aggregate verification is stable; a single flaky pass is not enough.
- Delegate code edits to execution-focused subagents; use planning/review-focused agents for planning, review, and thinking.
- When orchestrating parallel agents, the lead dictates exact scoped edits, subagents execute, and the lead reviews diffs before any push.
- Subagents should not invent scope beyond what the lead dictated.
- For Cursor plugin fixes, dogfood the official TraceDecay install or upgrade flow instead of hand-editing installed plugin files.

## Git

- Every non-merge commit subject must pass `scripts/check-conventional-commits.sh` before push.
- Use `<type>: <subject>` or `<type>(<scope>): <subject>` with one of:
  `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`.
- Keep the subject at 72 characters or fewer. Example: `fix(doctor): avoid false orphan warnings`.

## Learned Workspace Facts

- Parallel branch work uses git worktrees under `.worktrees/` in the repo root (for example `.worktrees/codex-cli-args-stdin`).
- Integration/default branch is `master` (GitHub: ScriptedAlchemy/tracedecay).
- Cursor's TraceDecay plugin uses the MCP key `tracedecay`; Claude and Codex retain the `graph` key.
- Multi-PR merge verification: build a detached temporary worktree on
  `origin/master`, merge all target branches, then run ordinary Cargo tests and
  let the local shim allocate the isolated build and test-data lane.
