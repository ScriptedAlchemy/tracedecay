# Claude Notes

## Cargo

- Do not commit an absolute `[build].target-dir`; hosted CI and published packages cannot assume `/scratch`.
- On this machine, keep build artifacts on scratch through global Cargo config or an ignored local symlink: `target -> /scratch/cargo-target/tracedecay`.
- Cargo-launched TraceDecay processes use `target/test-profile/.tracedecay`; with the local symlink, that also lands on scratch.
- Prefer plain Cargo commands from the repo root: `cargo check`, `cargo test`, `cargo test-all`, `cargo nextest run --workspace --no-fail-fast`.
- Do not override to shared `/scratch/cargo-target`; keep this repo isolated.
- On machines without `/scratch` and local config pointing there, run with explicit local overrides:

```sh
CARGO_TARGET_DIR=target TRACEDECAY_DATA_DIR=target/test-profile/.tracedecay cargo check
```

- In CI, set per-job paths explicitly:

```sh
CARGO_TARGET_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-cargo-target" \
TRACEDECAY_DATA_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-test-profile/.tracedecay" \
cargo test-all
```
