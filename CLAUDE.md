# Claude Notes

## Cargo

- These rules describe Zack's machine-local development environment, not
  TraceDecay product behavior, public contributor setup, or hosted CI.
- Run ordinary `cargo` commands. The machine-local shim allocates concurrent
  build lanes; do not set `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` yourself.
- Do not add `--locked` to local or agent Cargo commands. Existing CI,
  packaging, and `cargo install` commands may require lockfile reproducibility.
- Scope development checks narrowly. Before handoff, run the relevant
  all-feature gate: `cargo check --all-features`, `cargo test --all-features`,
  `cargo test-all`, or
  `cargo nextest run --workspace --all-features --no-fail-fast`.
- Keep repository Cargo configuration portable. Never encode machine-local
  `/fast` paths or lane policy in product code, public documentation, or CI.
