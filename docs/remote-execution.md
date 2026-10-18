# Private Bazel execution

Bazel can use a private Remote Execution API service without changing the
repository's ordinary local builds. Put the pool address and capacity in the
checkout's ignored `.bazelrc.local`, which `.bazelrc` imports last:

```bazelrc
build --config=rbe-linux
build --remote_executor=grpc://YOUR_PRIVATE_SCHEDULER:50051
build --remote_instance_name=main
build --remote_local_fallback=false
```

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
and Git, must also be installed. Test suites that spawn Node, ast-grep, or
other host tools need those tools on the worker as well.

Verify the real workload with local fallback disabled:

```sh
bazel test //scripts:rust_test_environment_probe --test_output=errors
bazel test //crates/tracedecay-code-extraction:unit_test --test_output=errors
bazel test //crates/tracedecay-query:index_bench_workload --test_output=errors
```

The first target exercises the isolated test launcher and declared Rust
toolchain, the second compiles and runs the native grammars, and the third
reads the committed corpus from test runfiles after changing directories.
Use Bazel's execution log or the scheduler's worker metrics to confirm which
machine executed actions; a remote cache hit alone does not prove execution.
