# Cargo build-directory policy

TraceDecay development uses ordinary stock Cargo. A normal checkout builds
artifacts into its repo-local `target/` directory:

```sh
cargo check
cargo test
cargo clippy --workspace --all-targets
```

This default is portable. Cargo safely serializes concurrent commands that
share a target directory, so a
`Blocking waiting for file lock on build directory` message means another
build owns that directory; it does not indicate database corruption or a
stalled TraceDecay process.

## Contended checkouts

Let Cargo wait for its build-directory lock when commands overlap. Do not
redirect `CARGO_TARGET_DIR` or `TRACEDECAY_DATA_DIR` merely to avoid contention;
doing so fragments incremental artifacts and can bypass the repository's
test-profile isolation.

TraceDecay diagnostic commands manage their own private target directories.
Do not reuse or delete those directories while a diagnostic command is active.

## Repository rules

- Do not commit an absolute `[build].target-dir` or any host-specific build
  path.
- Keep `.cargo/config.toml` portable. Its checked-in `target-dir = "target"`
  is relative to each checkout.
- Source, tests, documentation, and CI must invoke ordinary Cargo commands and
  must not require a developer-specific wrapper or shim.
- CI may select a runner-local target directory or cache through its own
  environment; that configuration must not leak into published packages or
  contributor setup.

## Verification

Before submitting a build-configuration change:

```sh
cargo check --workspace --all-targets
cargo test --workspace
```

Confirm that a fresh shell with a standard Rust toolchain can run the commands
without machine-local aliases, wrappers, or paths.
