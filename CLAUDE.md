# Claude Notes

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
- Run normal repo commands from the repo root with all features: `cargo check --all-features`, `cargo test --all-features`, `cargo test-all`, `cargo nextest run --workspace --all-features --no-fail-fast`.
- Toolchain caches (`sccache`, cargo registry) live under `/fast/cache/` and need no
  per-agent changes.
- CI is unchanged and keeps runner-local paths:

```sh
CARGO_TARGET_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-cargo-target" \
TRACEDECAY_DATA_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-test-profile/.tracedecay" \
cargo test-all
```
