# Native Blaze through Bazel

For a normal edit loop, keep the package, features and command stable so the
same daemon worker can reuse its compiler state:

```sh
bazel run //scripts/bazel/blaze:blaze -- test -p tracedecay-private-fs --lib
bazel run //scripts/bazel/blaze:blaze -- build -p tracedecay-cli --bin tracedecay
bazel run //scripts/bazel/blaze:blaze -- run -p tracedecay-cli --bin tracedecay -- --version
bazel run //scripts/bazel/blaze:blaze -- status
bazel run //scripts/bazel/blaze:blaze -- stop
```

Full workspace validation loads much more state. Use a host with measured memory
headroom, especially when it also runs NativeLink workers:

```sh
features=tracedecay/test-helpers,tracedecay/test-transport,tracedecay/search-eval,tracedecay-cli/test-transport,tracedecay-query/production
bazel run //scripts/bazel/blaze:blaze -- build --workspace --bins --features "$features"
bazel run //scripts/bazel/blaze:blaze -- build -p tracedecay-cli --example tracedecay-host-cli-fixture --features "$features"
bazel run --run_env=BLAZE_TESTS=all //scripts/bazel/blaze:blaze -- test --workspace --features "$features" --no-fail-fast
```

The target launches upstream's native Cargo driver from the checkout root. Cargo
owns workspace membership, features, build scripts, test selection and generated
artifacts; this integration has no second package list or compiler graph. Arguments
pass through unchanged except that `build`, `test`, `run` and `check` default to
`--locked` and load the existing `.cargo/acquisition.toml` with `--config`. The
launcher also exports that file's acquisition settings for the pinned engine's
metadata subprocesses, which do not forward `--config`. This does not force the
Cargo-only path. Native diagnostics,
fallback explanations and exit status pass through.
These commands also use Bazel's `RUSTC_BOOTSTRAP=1` environment so pinned stable
Cargo honors that policy's unstable shallow-dependency setting.
Development and test profiles compile every workspace crate and dependency at
optimization level 0. This also lets Blaze reuse rustc's incremental executable
for workspace edits. Release jobs select the optimized `dist` configuration.
Use Cargo's package/target/feature flags for focused runs. Required integration-test
features still apply; `--workspace` alone does not enable them.

The `@blaze` repository provisions `blaze`, `ra-jit`, `ra-jit-rustc` and
`ra-jit-rustc-first` together from upstream commit
`d44ef985e45e69661fb44696d61a18a251a39b08`. Its source archive is SHA-256 pinned.
The rules_rust Cargo bootstrap helper builds the four binaries with `--locked
--release`, shared artifacts, and Bazel's pinned host Rust compiler. This first
provisioning compiles upstream and requires its native build prerequisites and
registry access. It does not use the older nightly release asset. The optional
`blaze-proc-macro-srv` is not provisioned: upstream uses rustc to compile procedural
macros and the Rust toolchain server to expand them in this configuration.

For local validation against an already built checkout of **that exact commit**,
provide an absolute directory containing all four binaries:

```sh
bazel run --repo_env=TRACEDECAY_BLAZE_BIN_DIR="$BLAZE_CHECKOUT/target/release" \
  //scripts/bazel/blaze:blaze -- test -p tracedecay-private-fs --lib
```

This explicit repository override skips provisioning, declares and watches those
four binaries, and makes no provenance assertion about the supplied directory.
Omit it to use the pinned source build. Keep the same override on subsequent
invocations to use the same tool identity.

## Runtime and CI caches

Defaults are absolute paths rooted in the checkout:

| Purpose | Directory | Configuration |
| --- | --- | --- |
| Cargo artifacts and native build metadata | `target/blaze/cargo` | `CARGO_TARGET_DIR`, or Cargo `--target-dir` |
| Compiled code store | `target/blaze/store` | `BLAZE_CACHE` / `RA_JIT_CACHE` |
| Daemon socket, locks, logs, workers, downloaded toolchain | `target/blaze/home` | `BLAZE_HOME` |

Upstream translates `BLAZE_HOME` to `RA_JIT_HOME`, overriding a separately supplied
`RA_JIT_HOME`. The launcher therefore uses one home for both purposes; an explicit
`RA_JIT_HOME` alone also becomes this shared home. Keep Unix socket paths short
enough for the host. `BLAZE_CACHE` takes precedence over `RA_JIT_CACHE`, as upstream
defines. Cargo's ordinary `target/` artifacts remain separate by default.

CI can restore/save `target/blaze/cargo` and `target/blaze/store` between matching
toolchain, upstream pin, platform, dependency and feature configurations. Stop the
daemon before saving caches. Do not cache live sockets, locks or workers from
`target/blaze/home`; an upstream-downloaded toolchain can be cached specifically
under `target/blaze/home/toolchains`. A daemon restart loses its in-memory state;
cross-job disk-cache reuse and same-daemon edit reuse are distinct measurements.
The Bazel repository download cache retains the source archive, not the compiled
tool. The bootstrapped binaries persist only with that Bazel output base; a fresh
CI output base rebuilds upstream unless CI deliberately restores a compatible
compiled tool directory and selects it with `TRACEDECAY_BLAZE_BIN_DIR`. Neither
Bazel's action disk cache nor the workspace code store avoids this bootstrap.
Cargo's registry cache is separate. No TraceDecay speedup is claimed by this
launcher.

Blaze resolves a complete Rust 1.99.0 toolchain with `rust-src` from `RUSTC`, rustup,
or its own checksum-pinned download. `bazel run //scripts/bazel/blaze:blaze --
toolchain` reports the selection. Bazel's `cargo_host_tools` provisions the compiler
used to build Blaze; it lacks `rust-src` and is not passed off as the complete
runtime toolchain. An explicit incomplete `RUSTC` is rejected by upstream.
When `CARGO` names an absolute executable, its directory also leads the launched
process's `PATH`: upstream's unit-graph loader invokes bare `cargo`, and a
different shell broker can otherwise discard its JSON output and force fallback.

## Correctness and workspace prerequisites

Normal Bazel Rust compilation and tests remain the correctness authority and the
source of canonical artifacts. Blaze's incremental diagnostics do not fully
replace rustc's borrow checking. This target does not replace existing correctness
jobs, release builds or published artifacts.

Native Cargo uses manifest sources and does **not** apply `MODULE.bazel`'s
asupersync scheduler and blocking-pool stack patches, AWS include-path patch, or
arborium runtime-manifest-directory patches. A native Cargo result is not evidence
of behavior under those patched Bazel dependencies. Keep this distinction explicit
when comparing failures, performance and artifacts.

The existing `.cargo/config.toml` remains active, including its forced workspace
test profile and global-database isolation. Build scripts still require their
real inputs and native tools. The CLI dashboard embedder either builds the
dashboard (requiring the JavaScript prerequisites) or accepts a prebuilt bundle
only with `TRACEDECAY_SKIP_DASHBOARD_BUILD=1` and a valid
`TRACEDECAY_DASHBOARD_BUNDLE_SHA256`. The launcher does not fabricate generated
inputs, weaken bundle checks, suppress failures or change test policy.

## Memory, edit loops, and upstream recipes

The launcher defaults `BLAZE_SPARE=0`: Cargo finishes its cold build before Blaze
loads the compiler database. An explicit `BLAZE_SPARE` or legacy `RA_JIT_SPARE`
overrides it. This reduces overlapping cold-build memory; it does not reduce the
memory needed by a fully loaded workspace. CI also limits concurrent Cargo jobs
to two. Neither setting is an overall memory cap.

`BLAZE_MEMORY_MAX` evicts eligible idle workers. Upstream never evicts a loading,
busy, or most-recently-used worker to meet that budget. `BLAZE_OWN_BASE_MEMORY`
only bounds the optional background base compilation. For actual containment,
use the host's cgroup limits and budget agents, Blaze, NativeLink and the desktop
together. A 16 GiB host cannot safely promise independent 11.5 GiB agent and
10 GiB worker allowances. Use focused packages there; measure the complete CLI's
working set before choosing it for a full-workspace daemon.

Daemon settings (home, store, spare, memory and idle timeout) are read at startup.
Stop the existing daemon before changing them, once its active work finishes.
For worktrees that should share a daemon and code store, set the same absolute
`BLAZE_HOME` and `BLAZE_CACHE` in host Bazel run settings. Keep `CARGO_TARGET_DIR`
separate per checkout unless Cargo artifact sharing has been explicitly arranged.
The Mac's configured paths stay on `bigssd`; no storage relocation is required.

For rebuild-on-save, an existing watcher can run the same focused Bazel command.
Do not enable Blaze as a global Cargo wrapper: the explicit driver preserves our
Bazel entrypoint and avoids upstream's slower global-wrapper path. Live process
patching requires application integration with `ra-jit-live`; it is separate from
incremental build support and is not enabled in TraceDecay's daemon.

The pinned source bootstrap is upstream's ordinary release build, not its
PGO-trained distribution recipe. Benchmark that exact tool build before claiming
a speedup. Keep the ordinary Bazel correctness jobs: Blaze's checks do not replace
rustc's full borrow checking. `BLAZE_TESTS=all` remains the CI policy; affected-only
tests are an optional local experiment, not an acceptance shortcut.

Blaze's [CI runner recipe](https://github.com/indexable-inc/blaze/blob/d44ef985e45e69661fb44696d61a18a251a39b08/docs/ci-runner.md)
is a design document, not a shipped NativeLink execution backend. Wrapping Blaze
in `bazel run` does not send its internal Cargo/rustc or JIT work to our fleet.
Ordinary Bazel builds retain that remote execution path. Disk caches also cannot
restore a daemon's in-memory analysis on a fresh GitHub runner.

Upstream references for the pinned implementation:
[usage](https://github.com/indexable-inc/blaze/blob/d44ef985e45e69661fb44696d61a18a251a39b08/docs/usage.md),
[Cargo and memory controls](https://github.com/indexable-inc/blaze/blob/d44ef985e45e69661fb44696d61a18a251a39b08/docs/cargo.md),
[cache formats and trust](https://github.com/indexable-inc/blaze/blob/d44ef985e45e69661fb44696d61a18a251a39b08/docs/cache.md),
and [installation](https://github.com/indexable-inc/blaze/blob/d44ef985e45e69661fb44696d61a18a251a39b08/docs/install.md).
