# Private Bazel execution

The repository always expands its `tracedecay` Bazel configuration, which
selects the pinned Rust toolchain. Add the following to the host's
`~/.bazelrc` to extend that profile with private execution settings:

```bazelrc
build:tracedecay --config=rbe-linux
build:tracedecay --config=fleet
build:fleet --remote_executor=grpc://YOUR_PRIVATE_SCHEDULER:50051
build:fleet --remote_cache=grpc://YOUR_PRIVATE_SCHEDULER:50051
build:fleet --remote_instance_name=main
build:fleet --remote_local_fallback
```

Every new TraceDecay worktree inherits this host profile automatically. Other
repositories do not select it. Without a host extension, including under
`--nohome_rc` in CI, the repository still selects the pinned toolchain and
builds locally.
The ignored `.bazelrc.local` remains available for later per-checkout overrides;
command-line options take precedence over rc settings.

## Workstation and remote CPUs share one scheduler

The repository's `fleet` profile sends Rust compilation and test actions to
NativeLink. A workstation enrolled as a NativeLink worker contributes its CPU
and memory alongside the other workers. In this setup, Bazel's `remote` label
means execution through the shared scheduler; it does not mean execution only
on another physical machine. Ubuntu-main coordinates the pool, while both
Ubuntu-main and the enrolled Omarchy workstation can execute Linux actions.

Use the normal `bazel build` and `bazel test` commands with the host profile
above. No per-command Rustc or TestRunner strategy overrides are needed. The
scheduler admits actions against each worker's advertised capacity and action
requirements. Keep CPU counts, memory limits, in-flight action limits, private
addresses, and the client's `--jobs` queue depth in host configuration. A queue
deeper than the available worker slots lets the shared scheduler distribute
ready actions; it does not increase the workers' admission limits. Dependency
chains and memory-intensive links can still leave CPUs idle.

Do not add a competing local dynamic branch on a workstation whose cores are
already enrolled in that pool: Bazel and NativeLink would independently admit
work against the same machine. Dynamic execution is useful for a client that
is not itself a fleet worker, such as a GitHub-hosted runner. That client's
later `.bazelrc.local` can select dynamic strategies with its own resource
limits, as the CI setup does for tests. Dynamic execution races duplicate local
and remote copies; it is not a partition of work between the machines.

Verify distribution using executed-action worker identities or worker metrics,
not the coordinator address, cache hits, or Bazel's aggregate `remote` count.
Changing a worker's advertised capacity requires measurement on that host;
repository settings cannot safely infer its available CPU or RAM.

Use `rbe-linux` on Linux x86_64 clients, or `rbe-macos` on Apple Silicon macOS
clients. Windows x86_64 clients use `rbe-windows` with MSVC 14.44.35207
mapped to `V:` and Windows SDK 10.0.26100.0 installed on both client and worker.
CI offloads Windows Rust compilation/linking while running build scripts and
tests on its hosted runner. These configurations attach `OSFamily` and `cpu_arch` execution
properties and matching Bazel constraints to the host platform. The scheduler
must match those properties exactly; Linux and macOS binaries require their
respective execution hosts. Set `--jobs` in the local configuration when the
pool's capacity calls for a limit. Bazel otherwise chooses its normal local
resource limits.

The Linux configuration selects the pinned
[`hermetic_cc_toolchain`](https://github.com/uber/hermetic_cc_toolchain)
compiler, headers, linker, and glibc 2.31 baseline. This allows Ubuntu and Arch
workers to execute the same C/C++ and Rust linking actions without depending
on the client's system compiler. Its writable compiler cache is
`/tmp/zig-cache`. The macOS configuration retains the native Apple toolchain
and SDK; macOS workers must provide the same Xcode and SDK installation as the
client. Worker system tools used by build scripts, such as `sh`, CMake, Perl,
and Git, must also be installed. Rust tests carry pinned Node, Rust Analyzer,
compiler libraries, and Rust sources in their declared runfiles. Test launchers
create private home and tool-state directories under `TEST_TMPDIR`; they do
not use the initiating machine's home or rustup installation. Remote Rust tests
reserve four CPU units and default to four harness threads.

Linux process isolation must mount `/proc` for the action's PID namespace.
A private PID namespace paired with host procfs makes process identity and
open-file checks incorrect. The action also needs normal signal delivery and
orphan reaping; an ordinary test executable should not become PID 1. Verify
these properties when configuring a worker, including action timeout cleanup.

Use a short, dedicated action work directory. The complete daemon socket path,
including the action UUID, temporary fixture home, and `.tracedecay/daemon.sock`,
must be shorter than 104 bytes on macOS and 108 bytes on Linux. Validate the
full path from an executed test; checking only the work directory misses this
limit. Worker sandboxes must allow Unix socket bind/connect inside the owned
action directories. On macOS, give the CAS and action trees matching sandbox
rights, including Unix socket permissions: macOS rejects hardlinks between
paths with different sandbox rights even when both allow file reads and writes.
Verify a CAS-to-action hardlink under the actual worker profile before starting
the worker. Scope these permissions to the worker-owned trees.

Keep action `TMPDIR` on the same writable filesystem and mount as
compiler outputs: a separate temporary mount can make Rust's artifact renames
fail with `EXDEV`. The action root may be separate from the CAS cache, but must
remain on the intended build volume.

NativeLink's macOS directory cache uses APFS `clonefile` automatically, with a
hardlink fallback. For many actions sharing a large toolchain, enable
`directory_cache.experimental_subtree_caching` so unchanged subtrees can be
prepared once and cloned across different action roots. Size `max_entries` and
`max_size_bytes` for subtree entries as well as roots; nested entries count their
logical sizes even when APFS shares their physical blocks. Confirm nonzero
`clonefile_hits` and `subtree_hits` in the directory-cache summary on a real run.
When input preparation stalls with little network activity, inspect local disk
throughput and the external drive's negotiated USB speed before tuning RPC limits.

See [CI fleet configuration](ci-remote-builds.md) for hosted runners, dynamic
execution, and restricted Tailscale enrollment.

Verify the real workload with local fallback and cached results disabled:

```sh
bazel test //scripts:rust_test_environment_probe --noremote_local_fallback --noremote_accept_cached --nocache_test_results --test_output=errors
bazel test //crates/tracedecay-code-extraction:unit_test --noremote_local_fallback --noremote_accept_cached --nocache_test_results --test_output=errors
bazel test //crates/tracedecay-query:index_bench_workload --noremote_local_fallback --noremote_accept_cached --nocache_test_results --test_output=errors
```

The first target exercises isolated homes, declared Node, a real Rust Analyzer
LSP request, and offline compiler tests/doctests; the second compiles and runs the native grammars, and the third
reads the committed corpus from test runfiles after changing directories.
Use Bazel's execution log or the scheduler's worker metrics to confirm which
machine executed actions; a remote cache hit alone does not prove execution.

The `code_index_chunks --run` benchmark defaults to
`TEST_UNDECLARED_OUTPUTS_DIR/result-provisional.json` under `bazel test`, so
Bazel retrieves the result with the test's undeclared outputs. Under
`bazel run` it writes to
`benchmark_data/query-code-index/result-provisional.json` in the source
workspace. An explicit `--output PATH` always selects the destination.
