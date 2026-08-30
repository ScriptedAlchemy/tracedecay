# Cargo build-directory policy

TraceDecay development invokes Cargo through the machine-local `kache cargo --`
front-end. `kache` execs the real Cargo after collapsing the duplicate
`$CARGO_HOME` rustflags source and isolating the per-worktree build directory
so builds hit the shared compile cache:

```sh
kache cargo -- check
kache cargo -- test
kache cargo -- clippy --workspace --all-targets
```

`kache` is compile-cache practice on this machine. It is not product, CI, or
release architecture, and it is not a revival of the rejected `cargo-slot`
shim. Stock `cargo` remains the portable command for a fresh checkout, CI, and
published contributor instructions.

Cargo safely serializes concurrent commands that share a target directory, so a
`Blocking waiting for file lock on build directory` message means another
build owns that directory; it does not indicate database corruption or a
stalled TraceDecay process.

## Contended checkouts

Let Cargo wait for its build-directory lock when commands overlap. Do not invent
a per-lane or `/tmp/...` `CARGO_TARGET_DIR` merely to avoid contention, and do
not redirect `TRACEDECAY_DATA_DIR` for that reason. Those redirects fragment
incremental artifacts and can bypass the repository's test-profile isolation.
`kache` already isolates the per-worktree build directory; the shared cache key
is profile × features × `RUSTFLAGS` × source, not the worktree path.

TraceDecay diagnostic commands manage their own private target directories.
Do not reuse or delete those directories while a diagnostic command is active.
Do not reclaim or wipe the machine kache store.

## Repository rules

- Do not commit an absolute `[build].target-dir` or any host-specific build
  path.
- Keep `.cargo/config.toml` portable. Its checked-in `target-dir = "target"`
  is relative to each checkout.
- Do not add a cargo-slot, lock-stealing shim, or any wrapper that changes
  Cargo semantics, feature resolution, or `RUSTFLAGS`. `kache cargo --` is the
  one allowed front-end because it execs stock Cargo and keeps the cache key
  stable.
- Novel feature permutations recompile the workspace spine. Stick to the
  standard lanes in `AGENTS.md`.
- CI may select a runner-local target directory or cache through its own
  environment; that configuration must not leak into published packages or
  require `kache` for a contributor.

## Verification

Before submitting a build-configuration change:

```sh
kache cargo -- check --workspace --all-targets
kache cargo -- test --workspace
```

Confirm that a fresh shell with a standard Rust toolchain can still run
ordinary `cargo` commands without machine-local aliases, wrappers, or paths.
