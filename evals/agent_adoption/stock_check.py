"""Execute trusted stock/order assertions against an agent's captured Rust sources.

The agent cannot edit the assertions or BUILD file. Bazel remains the compiler
and test authority; missing tooling or a failed build is unverified, never a
successful edit. Temporary packages live in the workspace only for this call.
"""
from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path, PurePosixPath

HERE = Path(__file__).resolve().parent
BUILD = '''load("@rules_rust//rust:defs.bzl", "rust_library", "rust_test")

rust_library(
    name = "orders_fixture",
    srcs = glob(["src/**/*.rs"]),
    crate_root = "src/lib.rs",
    edition = "2021",
    tags = ["manual"],
)

rust_test(
    name = "stock_check",
    srcs = ["stock_check.rs"],
    crate_root = "stock_check.rs",
    edition = "2021",
    deps = [":orders_fixture"],
    use_libtest_harness = False,
    size = "small",
    tags = ["manual"],
)
'''


def verify(effects: dict) -> dict:
    """Return pass/fail only for observed behavior; otherwise explain unverified."""
    if not isinstance(effects, dict) or not isinstance(effects.get("src/lib.rs"), str):
        return {"status": "fail", "reason": "the captured crate has no src/lib.rs"}
    for name, source in effects.items():
        if not isinstance(name, str):
            return {"status": "unverified", "reason": "captured source path is not a string"}
        path = PurePosixPath(name)
        if (not isinstance(source, str) or not name.startswith("src/")
                or path.suffix != ".rs" or ".." in path.parts or "\\" in name):
            return {"status": "unverified", "reason": f"invalid captured source path: {name!r}"}
    root = HERE.parent.parent
    if not (root / "MODULE.bazel").is_file():
        return {"status": "unverified", "reason": "stock oracle requires the repository's Bazel workspace"}
    bazel = shutil.which("bazel")
    if bazel is None:
        return {"status": "unverified", "reason": "Bazel is unavailable for the stock behavior oracle"}
    try:
        with tempfile.TemporaryDirectory(prefix="_stock_check_", dir=HERE) as tmp:
            staging = Path(tmp)
            for name, source in effects.items():
                path = staging / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            (staging / "BUILD.bazel").write_text(BUILD)
            shutil.copyfile(HERE / "stock_check.rs", staging / "stock_check.rs")
            label = "//" + staging.relative_to(root).as_posix() + ":stock_check"
            result = subprocess.run(
                [bazel, "test", label, "--test_output=errors", "--color=no"],
                cwd=root, capture_output=True, text=True, timeout=600,
            )
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"status": "unverified", "reason": f"could not execute stock behavior oracle: {error}"}
    # Bazel documents exit 3 as a successful build with failed/timed-out tests.
    # Build/toolchain errors cannot establish the candidate's runtime behavior.
    # https://bazel.build/run/scripts#what-exit-code-will-i-get
    status = {0: "pass", 3: "fail"}.get(result.returncode, "unverified")
    return {
        "status": status,
        "reason": {"pass": "stock and order behavior verified",
                   "fail": "stock or order behavior failed",
                   "unverified": "Bazel could not build and execute the stock behavior oracle"}[status],
        "exit_code": result.returncode,
        "output": result.stdout + result.stderr,
    }
