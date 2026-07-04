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
