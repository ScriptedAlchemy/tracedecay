"""Run the declared native Blaze Cargo driver against the calling checkout."""

import os
from pathlib import Path
import sys
import tomllib

from python.runfiles import runfiles


def main():
    workspace = os.environ.get("BUILD_WORKSPACE_DIRECTORY")
    if not workspace or not Path(workspace).is_absolute():
        raise ValueError("use bazel run //scripts/bazel/blaze:blaze -- <command> [flags]")
    workspace = Path(workspace).resolve(strict=True)
    if not (workspace / "Cargo.toml").is_file():
        raise ValueError(f"workspace has no Cargo.toml: {workspace}")
    files = runfiles.Create()
    location = os.environ.get("TRACEDECAY_BLAZE_EXECUTABLE")
    if files is None or not location:
        raise ValueError("Bazel has not supplied the native Blaze runfiles")
    resolved = files.Rlocation(location)
    if resolved is None:
        raise ValueError(f"native Blaze runfile is unavailable: {location}")
    executable = Path(resolved).resolve(strict=True)
    suffix = executable.suffix if os.name == "nt" else ""
    for companion in ("ra-jit", "ra-jit-rustc", "ra-jit-rustc-first"):
        path = executable.with_name(companion + suffix)
        if not path.is_file():
            raise ValueError(f"native Blaze companion is unavailable: {path}")

    state = workspace / "target" / "blaze"
    env = os.environ.copy()
    # Upstream maps every BLAZE_* to RA_JIT_*, including HOME. Consequently
    # BLAZE_HOME is both the toolchain home and the daemon's runtime home.
    env.setdefault("BLAZE_HOME", env.get("RA_JIT_HOME") or str(state / "home"))
    if "BLAZE_CACHE" not in env:
        env.setdefault("RA_JIT_CACHE", str(state / "store"))
    env.setdefault("CARGO_TARGET_DIR", str(state / "cargo"))

    args = sys.argv[1:]
    if args and args[0] in {"build", "test", "run", "check"}:
        policy = workspace / ".cargo" / "acquisition.toml"
        if not policy.is_file():
            raise ValueError(f"Cargo acquisition policy is unavailable: {policy}")
        # Match Bazel's repository environment: stable Cargo otherwise ignores
        # the shared policy's unstable shallow-dependency acquisition setting.
        env["RUSTC_BOOTSTRAP"] = "1"
        # The pinned engine's workspace loader launches additional Cargo
        # metadata commands without forwarding --config. Keep acquisition
        # consistent for those children using values from the same policy.
        acquisition = tomllib.loads(policy.read_text())
        env["CARGO_NET_GIT_FETCH_WITH_CLI"] = str(
            acquisition["net"]["git-fetch-with-cli"]
        ).lower()
        env["CARGO_UNSTABLE_GIT"] = acquisition["unstable"]["git"]
        options = args[1:args.index("--")] if "--" in args else args[1:]
        if "--locked" not in options and "--frozen" not in options:
            args.insert(1, "--locked")
        args[1:1] = ["--config", str(policy)]
    os.chdir(workspace)
    # Native output, Cargo fallback explanations, signals and exit status pass
    # directly through; the launcher neither retries nor substitutes Cargo.
    os.execve(executable, [str(executable), *args], env)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        print(f"blaze: {error}", file=sys.stderr)
        sys.exit(2)
