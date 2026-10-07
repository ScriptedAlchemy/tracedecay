load("@rules_rust//rust:defs.bzl", _rust_test = "rust_test")

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
    data = list(kwargs.pop("data", []))
    for label in runfiles_env.values():
        if label not in data:
            data.append(label)
    _rust_test(
        name = name,
        env = dict({"TRACEDECAY_DISABLE_GLOBAL_DB": "1"}, **{
            variable: "$(rootpath {})".format(label)
            for variable, label in runfiles_env.items()
        }),
        data = data,
        tags = tags,
        **kwargs
    )
