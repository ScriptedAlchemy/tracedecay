# Private Bazel execution

The repository always expands its `tracedecay` Bazel configuration, which
selects the pinned nightly toolchain. Add the following to the host's
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
`--nohome_rc` in CI, the repository still selects nightly and builds locally.
The ignored `.bazelrc.local` remains available for later per-checkout overrides;
command-line options take precedence over rc settings.

Use `rbe-linux` on Linux x86_64 clients, or `rbe-macos` on Apple Silicon macOS
clients. These configurations attach `OSFamily` and `cpu_arch` execution
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
action directories. Keep action `TMPDIR` on the same writable filesystem and
mount as
compiler outputs: a separate temporary mount can make Rust's artifact renames
fail with `EXDEV`. The action root may be separate from the CAS cache, but must
remain on the intended build volume.

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
