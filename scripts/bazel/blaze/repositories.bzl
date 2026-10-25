"""Bazel-owned provisioning of the pinned upstream native Blaze Cargo driver."""

load("@rules_rust//cargo/private:cargo_bootstrap.bzl", "cargo_bootstrap")

_COMMIT = "d44ef985e45e69661fb44696d61a18a251a39b08"
_SHA256 = "ba3b5b7f8aad697750ff6820483fbde641be839adc84f4adeb566c8c89a1f37d"
_BINARIES = ["ra-jit", "blaze", "ra-jit-rustc", "ra-jit-rustc-first"]

def _blaze_repository_impl(ctx):
    suffix = ".exe" if "windows" in ctx.os.name.lower() else ""
    override = ctx.getenv("TRACEDECAY_BLAZE_BIN_DIR")
    if override:
        absolute = override.startswith("/") or (suffix and (
            override[1:3] in [":\\", ":/"] or override.startswith("\\\\")
        ))
        if not absolute:
            fail("TRACEDECAY_BLAZE_BIN_DIR must be an absolute directory")
        directory = ctx.path(override)
        for binary in _BINARIES:
            path = directory.get_child(binary + suffix)
            if not path.exists:
                fail("TRACEDECAY_BLAZE_BIN_DIR is missing {}".format(path))
            ctx.watch(path)
            ctx.symlink(path, "bin/" + binary + suffix)
    else:
        ctx.download_and_extract(
            url = "https://github.com/indexable-inc/blaze/archive/{}.tar.gz".format(_COMMIT),
            sha256 = _SHA256,
            stripPrefix = "blaze-" + _COMMIT,
        )
        tools = ctx.path(ctx.attr.host_tools).dirname.get_child("bin")
        # All four builds share Cargo's artifacts. The maintained bootstrap helper
        # supplies --locked and --release; upstream retains its own dependency graph.
        for binary in _BINARIES:
            built = cargo_bootstrap(
                repository_ctx = ctx,
                cargo_bin = tools.get_child("cargo" + suffix),
                rustc_bin = tools.get_child("rustc" + suffix),
                binary = binary,
                cargo_manifest = ctx.path("crates/ra-jit/Cargo.toml"),
                environment = {
                    "RA_JIT_GIT_SHA": _COMMIT,
                    "RUSTC_WORKSPACE_WRAPPER": "",
                    "RUSTC_WRAPPER": "",
                },
            )
            ctx.symlink(built, "bin/" + binary + suffix)

    ctx.file("BUILD.bazel", """
package(default_visibility = ["//visibility:public"])

filegroup(
    name = "driver",
    srcs = ["bin/blaze{suffix}"],
)

filegroup(
    name = "runtime",
    srcs = {binaries},
)
""".format(
        suffix = suffix,
        binaries = repr(["bin/" + binary + suffix for binary in _BINARIES]),
    ))

blaze_repository = repository_rule(
    implementation = _blaze_repository_impl,
    attrs = {
        "host_tools": attr.label(mandatory = True, allow_single_file = True),
    },
    environ = ["TRACEDECAY_BLAZE_BIN_DIR"],
    doc = "Build the native Blaze driver with Bazel's pinned host Rust tools.",
)
