# Claude Notes

## Cargo

- Do not commit an absolute `[build].target-dir`; hosted CI and published packages must use repo-local or runner-local paths.
- On this machine, use the normal repo-local `target` directory; `/scratch` is no longer configured for Cargo targets.
- Cargo-launched TraceDecay processes use `target/test-profile/.tracedecay`.
- Prefer plain Cargo commands from the repo root: `cargo check`, `cargo test`, `cargo test-all`, `cargo nextest run --workspace --no-fail-fast`.
- Do not override to a shared `CARGO_TARGET_DIR`; keep this repo isolated.
- If local environment overrides point elsewhere, run with explicit local overrides:

```sh
CARGO_TARGET_DIR=target TRACEDECAY_DATA_DIR=target/test-profile/.tracedecay cargo check
```

- In CI, set per-job paths explicitly:

```sh
CARGO_TARGET_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-cargo-target" \
TRACEDECAY_DATA_DIR="${RUNNER_TEMP:-/tmp}/tracedecay-test-profile/.tracedecay" \
cargo test-all
```
