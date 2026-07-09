# Claude Notes

## Cargo

- Do not commit an absolute `[build].target-dir`; hosted CI and published packages must use repo-local or runner-local paths.
- **On this machine every Cargo invocation MUST set a custom target dir on the fast NVMe
  volume: `CARGO_TARGET_DIR=/fast/cargo-target/<repo-or-worktree-name>` (e.g.
  `/fast/cargo-target/tracedecay`, `/fast/cargo-target/tracedecay-merge-check`). The
  repo-local `target/` directory is LOCKED — never build into it, never rely on it.**
- Never place target dirs under `/tmp`, `$HOME`, or anywhere on the root disk — slow volume,
  gets wiped or fills up. Toolchain caches (`sccache`, cargo registry) live under
  `/fast/cache/` and need no per-agent changes.
- Cargo-launched TraceDecay test data uses `<CARGO_TARGET_DIR>/test-profile/.tracedecay`;
  set `TRACEDECAY_DATA_DIR` alongside the target dir:

```sh
CARGO_TARGET_DIR=/fast/cargo-target/tracedecay-<checkout> \
TRACEDECAY_DATA_DIR=/fast/cargo-target/tracedecay-<checkout>/test-profile/.tracedecay \
cargo check
```

- Run normal repo commands from the repo root: `cargo check`, `cargo test`, `cargo test-all`, `cargo nextest run --workspace --no-fail-fast` — each with the env above.
- One target dir per checkout/worktree (keyed by its directory name): concurrent agents in
  different worktrees stay isolated while rebuilds inside one checkout stay warm.
- CI is unchanged and keeps runner-local paths:

```sh
CARGO_TARGET_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-cargo-target" \
TRACEDECAY_DATA_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-test-profile/.tracedecay" \
cargo test-all
```
