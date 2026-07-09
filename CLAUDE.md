# Claude Notes

## Cargo

- Do not commit an absolute `[build].target-dir`; hosted CI and published packages must use repo-local or runner-local paths.
- On this machine, use the normal repo-local `target` directory; `/scratch` is no longer configured for Cargo targets.
- Cargo-launched TraceDecay processes use `target/test-profile/.tracedecay`.
- Prefer plain Cargo commands from the repo root: `cargo check`, `cargo test`, `cargo test-all`, `cargo nextest run --workspace --no-fail-fast`.
- Do not override to a shared `CARGO_TARGET_DIR`; keep this repo isolated.
- If a custom `CARGO_TARGET_DIR` is genuinely needed (isolated merge checks, throwaway
  worktrees), it MUST live on the fast NVMe volume in the dedicated targets folder:
  `/fast/cargo-target/<repo-or-worktree-name>` — never under `/tmp` or `$HOME` (slow root
  disk). Checkouts under `/fast/projects/` are already on the fast disk, so repo-local
  `target` needs no override; toolchain caches live under `/fast/cache/`.
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
