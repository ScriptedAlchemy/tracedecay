"""Select the runtime artifact of a Rust shared library for host packaging."""

load("@rules_rust//rust:rust_common.bzl", "TestCrateInfo")

def _rust_shared_library_output_impl(ctx):
    # cdylib targets wrap their crate information because Rust cannot link
    # against them. DefaultInfo also includes Windows import/debug artifacts.
    crate = ctx.attr.library[TestCrateInfo].crate
    if crate.type != "cdylib":
        fail("library must be a Rust cdylib target")
    return [DefaultInfo(files = depset([crate.output]))]

rust_shared_library_output = rule(
    implementation = _rust_shared_library_output_impl,
    attrs = {
        "library": attr.label(mandatory = True, providers = [TestCrateInfo]),
    },
)
