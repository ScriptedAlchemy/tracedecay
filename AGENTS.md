# Agent Notes

## Cargo

- Do not commit an absolute `[build].target-dir`; hosted CI and published packages cannot assume `/scratch`.
- On this machine, keep build artifacts on scratch through global Cargo config or an ignored local symlink: `target -> /scratch/cargo-target/tracedecay`.
- Cargo-launched TraceDecay test data uses `target/test-profile/.tracedecay`; with the local symlink, that also lands on scratch.
- Run normal repo commands from the repo root: `cargo check`, `cargo test`, `cargo test-all`, `cargo nextest run --workspace --no-fail-fast`.
- Do not set `CARGO_TARGET_DIR=/scratch/cargo-target` for this repo; use a repo-specific directory to avoid cross-repo contention.
- If `/scratch` is unavailable and local config points there, override both paths for that command:

```sh
CARGO_TARGET_DIR=target TRACEDECAY_DATA_DIR=target/test-profile/.tracedecay cargo check
```

- CI should set an explicit per-job target dir, for example:

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

## Learned Workspace Facts

- Parallel branch work uses git worktrees under `.worktrees/` in the repo root (for example `.worktrees/codex-cli-args-stdin`).
- Integration/default branch is `master`, not `main` (GitHub: ScriptedAlchemy/tracedecay).
- Multi-PR merge verification: build a detached scratch worktree on `origin/master`, merge all target branches, then run tests with isolated `CARGO_TARGET_DIR` and `TRACEDECAY_DATA_DIR` paths.
