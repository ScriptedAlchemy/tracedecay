# Cargo build-directory policy

TraceDecay development uses stock Cargo. A normal checkout builds into its own
repo-local `target/` directory:

```sh
cargo check
cargo test
cargo clippy --workspace --all-targets
```

This default is portable and keeps separate worktrees isolated. Cargo safely
serializes concurrent commands that share a target directory, so a
`Blocking waiting for file lock on build directory` message means another
build owns that directory; it does not indicate database corruption or a
stalled TraceDecay process.

## Contended checkouts

Prefer running each independent task in its own checkout or worktree. Its
repo-local `target/` then remains both isolated and reusable.

If two builds must run concurrently from one checkout, give one invocation a
private target directory with Cargo's standard `CARGO_TARGET_DIR` variable:

```sh
CARGO_TARGET_DIR=../tracedecay-target-review cargo check
```

Choose a writable path appropriate for the host. Reuse it for related commands
to preserve incremental artifacts, and remove it when that cache is no longer
useful. Do not point two concurrent tasks at the same fallback directory.

TraceDecay diagnostic commands manage their own private target directories.
Do not reuse or delete those directories while a diagnostic command is active.

## Repository rules

- Do not commit an absolute `[build].target-dir` or any host-specific build
  path.
- Keep `.cargo/config.toml` portable. Its checked-in `target-dir = "target"`
  is relative to each checkout.
- Source, tests, documentation, and CI must invoke ordinary Cargo commands and
  must not require a developer-specific wrapper.
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
