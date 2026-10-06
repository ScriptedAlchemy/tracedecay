load("@rules_rust//rust:defs.bzl", _rust_test = "rust_test")

# Every test gets a private TraceDecay profile under TEST_TMPDIR and no global
# database. Variables named in `runfiles_env` carry an `$(rlocationpath)`
# that the launcher turns into an absolute runfiles path, because the binaries
# a suite spawns must stay addressable after it changes working directory.
_POSIX_LAUNCHER = """#!/bin/sh
set -eu
: "${{TEST_TMPDIR:?Bazel did not provide TEST_TMPDIR}}"
export TRACEDECAY_DATA_DIR="$TEST_TMPDIR/.tracedecay"
export TRACEDECAY_DISABLE_GLOBAL_DB=1
# Names such as CARGO_BIN_EXE_<bin> may carry `-`, which no shell variable
# can, so read with printenv and pass the absolute values through env.
exec env {runfiles_assignments} "$TEST_SRCDIR/$TEST_WORKSPACE/{binary}" "$@"
"""

_WINDOWS_LAUNCHER = """@echo off
setlocal EnableDelayedExpansion
if not defined TEST_TMPDIR (
  echo Bazel did not provide TEST_TMPDIR 1>&2
  exit /b 1
)
set "TRACEDECAY_DATA_DIR=%TEST_TMPDIR%/.tracedecay"
set "TRACEDECAY_DISABLE_GLOBAL_DB=1"
for %%N in ({runfiles_env}) do set "%%N=%TEST_SRCDIR%/!%%N!"
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
    ctx.actions.write(
        output = executable,
        content = template.format(
            binary = test_binary.short_path,
            runfiles_env = " ".join(ctx.attr.runfiles_env),
            runfiles_assignments = " ".join([
                "\"{name}=$TEST_SRCDIR/$(printenv '{name}')\"".format(name = name)
                for name in ctx.attr.runfiles_env
            ]),
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
        "runfiles_env": attr.string_list(),
        "test_binary": attr.label(
            executable = True,
            cfg = "target",
            mandatory = True,
        ),
        "_windows": attr.label(default = "@platforms//os:windows"),
    },
    test = True,
)

def rust_test(name, tags = [], runfiles_env = {}, **kwargs):
    """rules_rust's rust_test behind the isolating launcher.

    `runfiles_env` maps environment variable names to labels whose runfiles
    location the suite reads at run time; each label also joins `data`.
    """
    visibility = kwargs.pop("visibility", None)
    binary_name = name + "__binary"
    env = {
        "TRACEDECAY_DATA_DIR": "/dev/null",
        "TRACEDECAY_DISABLE_GLOBAL_DB": "1",
    }
    data = list(kwargs.pop("data", []))
    for variable, label in sorted(runfiles_env.items()):
        env[variable] = "$(rlocationpath {})".format(label)
        if label not in data:
            data.append(label)
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
        "runfiles_env": sorted(runfiles_env.keys()),
    }
    if visibility != None:
        wrapper_args["visibility"] = visibility
    _isolated_rust_test(**wrapper_args)
