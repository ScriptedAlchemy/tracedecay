# Agent Notes

## Cargo

- Do not commit an absolute `[build].target-dir`; hosted CI and published packages must use repo-local or runner-local paths.
- On this machine, use the normal repo-local `target` directory; `/scratch` is no longer configured for Cargo targets.
- Cargo-launched TraceDecay test data uses `target/test-profile/.tracedecay`.
- Run normal repo commands from the repo root: `cargo check`, `cargo test`, `cargo test-all`, `cargo nextest run --workspace --no-fail-fast`.
- Do not set a shared `CARGO_TARGET_DIR`; use the repo-local default or a repo-specific temporary directory.
- If local environment overrides point elsewhere, override both paths for that command:

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
- Integration/default branch is `master` (GitHub: ScriptedAlchemy/tracedecay).
- Multi-PR merge verification: build a detached temporary worktree on `origin/master`, merge all target branches, then run tests with isolated `CARGO_TARGET_DIR` and `TRACEDECAY_DATA_DIR` paths.
