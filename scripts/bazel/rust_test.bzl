"""Hermetic Rust tests for generated workspace targets."""

load("@rules_rust//rust:defs.bzl", _rust_test = "rust_test")


def _isolated_rust_test_impl(ctx):
    executable = ctx.actions.declare_file(ctx.label.name)
    test_binary = ctx.executable.test_binary
    ctx.actions.write(
        output = executable,
        content = """#!/bin/sh
set -eu
: "${{TEST_TMPDIR:?Bazel did not provide TEST_TMPDIR}}"
export TRACEDECAY_DATA_DIR="$TEST_TMPDIR/.tracedecay"
export TRACEDECAY_DISABLE_GLOBAL_DB=1
export CARGO_MANIFEST_DIR="$TEST_SRCDIR/$TEST_WORKSPACE/{package}"
exec "$TEST_SRCDIR/$TEST_WORKSPACE/{binary}" "$@"
""".format(
            binary = test_binary.short_path,
            package = ctx.attr.package,
        ),
        is_executable = True,
    )
    runfiles = ctx.runfiles(files = [test_binary]).merge(
        ctx.attr.test_binary[DefaultInfo].default_runfiles,
    )
    return [DefaultInfo(executable = executable, runfiles = runfiles)]


_isolated_rust_test = rule(
    implementation = _isolated_rust_test_impl,
    attrs = {
        "package": attr.string(mandatory = True),
        "test_binary": attr.label(
            executable = True,
            cfg = "target",
            mandatory = True,
        ),
    },
    test = True,
)


def rust_test(name, tags = [], **kwargs):
    """Compiles a Rust test and runs it with a Bazel-owned profile."""
    visibility = kwargs.pop("visibility", None)
    binary_name = name + "__binary"
    _rust_test(
        name = binary_name,
        env = {
            "TRACEDECAY_DATA_DIR": "/dev/null",
            "TRACEDECAY_DISABLE_GLOBAL_DB": "1",
        },
        tags = tags + ["manual"],
        visibility = ["//visibility:private"],
        **kwargs
    )
    wrapper_args = {
        "name": name,
        "package": native.package_name(),
        "tags": tags,
        "test_binary": ":" + binary_name,
    }
    if visibility != None:
        wrapper_args["visibility"] = visibility
    _isolated_rust_test(**wrapper_args)
