# Cargo invocation policy

Run plain `cargo <subcommand>` — cargo-conductor brokers every invocation
(PATH shim). Expect `[cargo-conductor] ticket cc-N` lines on stderr.

Do **not** prefix with `kache` (bypasses the broker), wrap in `flock`, or set
`CARGO_TARGET_DIR` / isolate builds. The broker serializes per target dir,
dedupes identical runs, and batches compatible checks. Kache caching still
applies automatically via the workspace `rustc-wrapper`. Prefer scoped
commands (`cargo check -p <crate> --lib`) — they coalesce and release early.

```sh
cargo check -p tracedecay-store --lib
cargo test -p tracedecay-store
cargo clippy -p tracedecay-store --all-targets
```

Never kill cargo processes. Use `conductor status` (not ps/pgrep) for queue
visibility. If a backgrounded ticket's result will not retrieve, rerun the
command (dedup makes it cheap) — known issue cargo-conductor#16.
`daemon unreachable; running cargo directly` is fail-open: proceed and mention
it. `kache monitor` is a cache dashboard, not a cargo front-end.

cargo-conductor and kache are machine-local practice. They are not product,
CI, or release architecture, and they are not a revival of the rejected
`cargo-slot` shim. Stock `cargo` remains the portable command for a fresh
checkout, CI, and published contributor instructions.

## Contended checkouts

The broker owns serialization. Do not invent a per-lane or `/tmp/...`
`CARGO_TARGET_DIR` merely to avoid contention, and do not redirect
`TRACEDECAY_DATA_DIR` for that reason. Those redirects fragment incremental
artifacts and can bypass the repository's test-profile isolation. The shared
compile-cache key is profile × features × `RUSTFLAGS` × source, not the
worktree path.

TraceDecay diagnostic commands manage their own private target directories.
Do not reuse or delete those directories while a diagnostic command is active.
Do not reclaim or wipe the machine kache store.

## Repository rules

- Do not commit an absolute `[build].target-dir` or any host-specific build
  path.
- Keep `.cargo/config.toml` portable. Its checked-in `target-dir = "target"`
  is relative to each checkout. Do not edit a machine-local
  `rustc-wrapper = "kache"` — that is the compile-cache layer, not a cargo
  prefix.
- Do not add a cargo-slot, lock-stealing shim, or any wrapper that changes
  Cargo semantics, feature resolution, or `RUSTFLAGS`. cargo-conductor execs
  stock Cargo; prefixing `kache cargo` bypasses the broker.
- Novel feature permutations recompile the workspace spine. Stick to the
  standard lanes in `AGENTS.md`.
- CI may select a runner-local target directory or cache through its own
  environment; that configuration must not leak into published packages or
  require cargo-conductor or `kache` for a contributor.

## Verification

Before submitting a build-configuration change:

```sh
cargo check --workspace --all-targets
cargo test --workspace
```

Confirm that a fresh shell with a standard Rust toolchain can still run
ordinary `cargo` commands without machine-local aliases, wrappers, or paths.
