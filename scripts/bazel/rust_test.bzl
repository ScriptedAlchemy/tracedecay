load("@rules_rust//rust:defs.bzl", "rust_clippy_aspect", _rust_test = "rust_test")

# Every test gets a private TraceDecay profile and Cargo target scratch dir
# under TEST_TMPDIR and no global database. Each `runfiles_env` variable is
# exported as an absolute runfiles path, because the binaries a suite spawns
# must stay addressable after it changes working directory.
_POSIX_LAUNCHER = """#!/bin/sh
set -eu
: "${{TEST_TMPDIR:?Bazel did not provide TEST_TMPDIR}}"
export RUSTUP_HOME="${{RUSTUP_HOME:-$HOME/.rustup}}"
export TRACEDECAY_TEST_RUSTUP_BIN="${{CARGO_HOME:-$HOME/.cargo}}/bin"
export CARGO_HOME="$TEST_TMPDIR/cargo-home"
export HOME="$TEST_TMPDIR/home"
mkdir -p "$HOME"
export TRACEDECAY_DATA_DIR="$TEST_TMPDIR/.tracedecay"
export TRACEDECAY_DISABLE_GLOBAL_DB=1
# Scratch may live below an operator home whose ancestor Cargo config names
# an undeclared cache wrapper. Fixture compilation uses Bazel's compiler.
export RUSTC_WRAPPER=""
export RUSTC_WORKSPACE_WRAPPER=""
export CARGO_TARGET_TMPDIR="$TEST_TMPDIR/cargo-target-tmp"
mkdir -p "$CARGO_TARGET_TMPDIR"
# Cargo runs a test binary from its crate directory, and suites read
# fixtures relative to it.
export CARGO_MANIFEST_DIR="$TEST_SRCDIR/$TEST_WORKSPACE/{package}"
cd "$CARGO_MANIFEST_DIR"
export PATH="{toolchain_path}:$PATH"
# Names such as CARGO_BIN_EXE_<bin> may carry `-`, which no shell variable
# can, so each assignment goes through env as its own argument: a runfiles
# path with spaces stays one word.
exec env {runfiles_env} "$TEST_SRCDIR/$TEST_WORKSPACE/{binary}" "$@"
"""

_WINDOWS_LAUNCHER = """@echo off
setlocal
if not defined TEST_TMPDIR (
  echo Bazel did not provide TEST_TMPDIR 1>&2
  exit /b 1
)
if not defined RUSTUP_HOME set "RUSTUP_HOME=%USERPROFILE%/.rustup"
if not defined CARGO_HOME set "CARGO_HOME=%USERPROFILE%/.cargo"
set "TEST_TMPDIR=%TEST_TMPDIR:/=\\%"
set "TRACEDECAY_TEST_RUSTUP_BIN=%CARGO_HOME%/bin"
set "CARGO_HOME=%TEST_TMPDIR%\\cargo-home"
set "HOME=%TEST_TMPDIR%\\home"
set "USERPROFILE=%HOME%"
if not exist "%HOME%" mkdir "%HOME%"
set "TRACEDECAY_DATA_DIR=%TEST_TMPDIR%\\.tracedecay"
set "TRACEDECAY_DISABLE_GLOBAL_DB=1"
set "CARGO_TARGET_TMPDIR=%TEST_TMPDIR%\\cargo-target-tmp"
if not exist "%CARGO_TARGET_TMPDIR%" mkdir "%CARGO_TARGET_TMPDIR%"
{runfiles_env}
set "PATH={toolchain_path};%PATH%"
set "TEST_PACKAGE=%TEST_SRCDIR%/%TEST_WORKSPACE%/{package}"
set "CARGO_MANIFEST_DIR=%TEST_PACKAGE%"
cd /d "%TEST_PACKAGE:/=\\%"
set "TEST_BINARY=%TEST_SRCDIR%/%TEST_WORKSPACE%/{binary}"
set "TEST_BINARY=%TEST_BINARY:/=\\%"
if not exist "%TEST_BINARY%" (
  echo test binary missing from the runfiles tree: %TEST_BINARY% 1>&2
  dir /s /b "%TEST_SRCDIR%" 1>&2
  exit /b 1
)
"%TEST_BINARY%" %*
exit /b %ERRORLEVEL%
"""

def _runfiles_path(file, workspace):
    """A file's path below TEST_SRCDIR: main-repository files sit under the
    workspace directory, external ones (`../<repo>/...`) under their repo."""
    if file.short_path.startswith("../"):
        return file.short_path[len("../"):]
    return "{}/{}".format(workspace, file.short_path)

def _isolated_rust_test_impl(ctx):
    test_binary = ctx.executable.test_binary
    toolchain = ctx.toolchains["@rules_rust//rust:toolchain_type"]
    windows = ctx.target_platform_has_constraint(
        ctx.attr._windows[platform_common.ConstraintValueInfo],
    )
    template = _WINDOWS_LAUNCHER if windows else _POSIX_LAUNCHER
    executable = ctx.actions.declare_file(
        ctx.label.name + (".bat" if windows else ""),
    )
    files_by_label = {}
    runfiles_env_files = []
    for target in ctx.attr.runfiles_env_targets:
        files = target[DefaultInfo].files.to_list()
        if len(files) != 1:
            fail("runfiles_env target {} must produce exactly one file".format(target.label))
        files_by_label[str(target.label)] = files[0]
        runfiles_env_files.append(files[0])
    workspace = "%TEST_WORKSPACE%" if windows else "$TEST_WORKSPACE"
    paths = {
        variable: _runfiles_path(files_by_label[label], workspace)
        for variable, label in ctx.attr.runfiles_env.items()
    }
    for variable, file in {"CARGO": toolchain.cargo, "RUSTC": toolchain.rustc, "RUSTDOC": toolchain.rust_doc}.items():
        paths[variable] = _runfiles_path(file, workspace)
    # Distribution acceptance explicitly supplies its extracted CLI through
    # --test_env. Every other runfile, and the default CLI, stays declared here.
    if windows:
        runfiles_env = "\n".join([
            ('if not defined {} '.format(variable) if variable == "TRACEDECAY_TEST_BIN" else "") +
            'set "{}=%TEST_SRCDIR%/{}"'.format(variable, path)
            for variable, path in paths.items()
        ])
    else:
        runfiles_env = " ".join([
            '"{}=${{{}:-$TEST_SRCDIR/{}}}"'.format(variable, variable, path)
            if variable == "TRACEDECAY_TEST_BIN" else
            '"{}=$TEST_SRCDIR/{}"'.format(variable, path)
            for variable, path in paths.items()
        ])
    # Production test runners invoke literal `cargo` and feedback fixtures
    # invoke literal `rustc`. Those must use the same declared toolchain as
    # CARGO/RUSTC, even after a child isolates HOME or changes directory.
    toolchain_bin = paths["RUSTC"].rsplit("/", 1)[0]
    toolchain_path = ("%TEST_SRCDIR%/" if windows else "$TEST_SRCDIR/") + toolchain_bin
    ctx.actions.write(
        output = executable,
        content = template.format(
            binary = test_binary.short_path,
            package = ctx.label.package,
            runfiles_env = runfiles_env,
            toolchain_path = toolchain_path,
        ),
        is_executable = True,
    )
    runfiles = ctx.runfiles(
        files = [test_binary] + runfiles_env_files,
        transitive_files = toolchain.all_files,
    ).merge(
        ctx.attr.test_binary[DefaultInfo].default_runfiles,
    )
    for target in ctx.attr.runfiles_env_targets:
        runfiles = runfiles.merge(target[DefaultInfo].default_runfiles)
    test_environment = ctx.attr.test_binary[RunEnvironmentInfo]
    environment = dict(test_environment.environment)
    environment["RUSTUP_TOOLCHAIN"] = toolchain.version
    environment["RUSTC_WRAPPER"] = ""
    environment["RUSTC_WORKSPACE_WRAPPER"] = ""
    return [
        DefaultInfo(executable = executable, runfiles = runfiles),
        OutputGroupInfo(
            clippy_checks = getattr(ctx.attr.test_binary[OutputGroupInfo], "clippy_checks", depset())
            if OutputGroupInfo in ctx.attr.test_binary else depset(),
        ),
        RunEnvironmentInfo(
            environment = environment,
            inherited_environment = test_environment.inherited_environment,
        ),
    ]

_isolated_rust_test = rule(
    implementation = _isolated_rust_test_impl,
    attrs = {
        "runfiles_env": attr.string_dict(),
        "runfiles_env_targets": attr.label_list(allow_files = True),
        "test_binary": attr.label(
            aspects = [rust_clippy_aspect],
            executable = True,
            cfg = "target",
            mandatory = True,
        ),
        "_windows": attr.label(default = "@platforms//os:windows"),
    },
    toolchains = ["@rules_rust//rust:toolchain_type"],
    test = True,
)

def perf_rustc_flags(opt_level):
    """The Cargo `perf` profile for one crate outside `-c opt`: its opt-level
    with debug assertions and overflow checks. `-c opt` (release) keeps the
    toolchain's opt-level 3 without them."""
    return select({
        "//:opt_mode": [],
        "//conditions:default": [
            "-Copt-level={}".format(opt_level),
            "-Cdebug-assertions=on",
            "-Coverflow-checks=on",
        ],
    })

def rust_test(name, tags = [], runfiles_env = {}, **kwargs):
    """rules_rust's rust_test behind the isolating launcher.

    `runfiles_env` maps environment variable names to labels whose absolute
    runfiles path the suite reads at run time; each label also joins `data`.
    """
    visibility = kwargs.pop("visibility", None)
    test_attributes = {
        attribute: kwargs.pop(attribute)
        for attribute in ["args", "size", "timeout", "flaky", "local", "shard_count"]
        if attribute in kwargs
    }
    binary_name = name + "__binary"
    env = {
        "TRACEDECAY_DATA_DIR": "/dev/null",
        "TRACEDECAY_DISABLE_GLOBAL_DB": "1",
    }
    data = list(kwargs.pop("data", []))
    present = [native.package_relative_label(entry) for entry in data]
    for label in runfiles_env.values():
        resolved = native.package_relative_label(label)
        if resolved not in present:
            present.append(resolved)
            data.append(label)
    if "crate" not in kwargs:
        kwargs.setdefault("crate_name", name.replace("-", "_"))
    _rust_test(
        name = binary_name,
        env = env,
        data = data,
        tags = tags + ["manual"],
        visibility = ["//visibility:private"],
        **kwargs
    )
    wrapper_args = {
        "name": name,
        "tags": tags,
        "test_binary": ":" + binary_name,
        "runfiles_env": {
            variable: str(native.package_relative_label(label))
            for variable, label in runfiles_env.items()
        },
        "runfiles_env_targets": sorted({
            native.package_relative_label(label): None
            for label in runfiles_env.values()
        }.keys()),
    }
    wrapper_args.update(test_attributes)
    if visibility != None:
        wrapper_args["visibility"] = visibility
    _isolated_rust_test(**wrapper_args)
