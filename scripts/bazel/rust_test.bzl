load("@rules_rust//rust:defs.bzl", _rust_test = "rust_test")

# Every test gets a private TraceDecay profile and Cargo target scratch dir
# under TEST_TMPDIR and no global database. Each `runfiles_env` variable is
# exported as an absolute runfiles path, because the binaries a suite spawns
# must stay addressable after it changes working directory.
_POSIX_LAUNCHER = """#!/bin/sh
set -eu
: "${{TEST_TMPDIR:?Bazel did not provide TEST_TMPDIR}}"
export TRACEDECAY_DATA_DIR="$TEST_TMPDIR/.tracedecay"
export TRACEDECAY_DISABLE_GLOBAL_DB=1
export CARGO_TARGET_TMPDIR="$TEST_TMPDIR/cargo-target-tmp"
mkdir -p "$CARGO_TARGET_TMPDIR"
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
set "TRACEDECAY_DATA_DIR=%TEST_TMPDIR%/.tracedecay"
set "TRACEDECAY_DISABLE_GLOBAL_DB=1"
set "CARGO_TARGET_TMPDIR=%TEST_TMPDIR%/cargo-target-tmp"
if not exist "%CARGO_TARGET_TMPDIR%" mkdir "%CARGO_TARGET_TMPDIR%"
{runfiles_env}
"%TEST_SRCDIR%/%TEST_WORKSPACE%/{binary}" %*
exit /b %ERRORLEVEL%
"""

def _isolated_rust_test_impl(ctx):
    test_binary = ctx.executable.test_binary
    windows = ctx.target_platform_has_constraint(
        ctx.attr._windows[platform_common.ConstraintValueInfo],
    )
    template = _WINDOWS_LAUNCHER if windows else _POSIX_LAUNCHER
    executable = ctx.actions.declare_file(
        ctx.label.name + (".bat" if windows else ""),
    )
    paths = {
        variable: ctx.expand_location(location, ctx.attr.data)
        for variable, location in sorted(ctx.attr.runfiles_env.items())
    }
    if windows:
        runfiles_env = "\n".join([
            'set "{}=%TEST_SRCDIR%/{}"'.format(variable, path)
            for variable, path in paths.items()
        ])
    else:
        runfiles_env = " ".join([
            '"{}=$TEST_SRCDIR/{}"'.format(variable, path)
            for variable, path in paths.items()
        ])
    ctx.actions.write(
        output = executable,
        content = template.format(
            binary = test_binary.short_path,
            runfiles_env = runfiles_env,
        ),
        is_executable = True,
    )
    runfiles = ctx.runfiles(files = [test_binary]).merge(
        ctx.attr.test_binary[DefaultInfo].default_runfiles,
    )
    test_environment = ctx.attr.test_binary[RunEnvironmentInfo]
    return [
        DefaultInfo(executable = executable, runfiles = runfiles),
        RunEnvironmentInfo(
            environment = test_environment.environment,
            inherited_environment = test_environment.inherited_environment,
        ),
    ]

_isolated_rust_test = rule(
    implementation = _isolated_rust_test_impl,
    attrs = {
        # Location expansion only: the test binary's runfiles carry the files.
        "data": attr.label_list(allow_files = True),
        "runfiles_env": attr.string_dict(),
        "test_binary": attr.label(
            executable = True,
            cfg = "target",
            mandatory = True,
        ),
        "_windows": attr.label(default = "@platforms//os:windows"),
    },
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
    binary_name = name + "__binary"
    env = {
        "TRACEDECAY_DATA_DIR": "/dev/null",
        "TRACEDECAY_DISABLE_GLOBAL_DB": "1",
    }
    data = list(kwargs.pop("data", []))
    present = [native.package_relative_label(entry) for entry in data]
    located = []
    for label in runfiles_env.values():
        resolved = native.package_relative_label(label)
        if resolved not in present:
            present.append(resolved)
            data.append(label)
        if resolved not in located:
            located.append(resolved)
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
            variable: "$(rlocationpath {})".format(label)
            for variable, label in runfiles_env.items()
        },
        "data": located,
    }
    if visibility != None:
        wrapper_args["visibility"] = visibility
    _isolated_rust_test(**wrapper_args)
