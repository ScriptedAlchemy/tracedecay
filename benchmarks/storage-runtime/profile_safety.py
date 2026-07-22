"""Live-profile isolation, filesystem checks, and child sandbox policy."""

from __future__ import annotations

import os
import platform
import sys
from pathlib import Path

from runner_contract import (
    ConfigError, NETWORK_FILESYSTEM_TYPES, PROTECTED_CHILD_ENV_KEYS,
    SCRUB_ENV_EXACT, SCRUB_ENV_PREFIXES, SafetyError,
)
from safe_paths import (
    _absolute, _normalized_path, _path_is_within, _windows_casefold,
    assert_safe_path_components, create_fresh_directory,
)

def _real(path_like) -> Path:
    # This is used only to compare a known live-profile root after the supplied
    # candidate has already passed lstat component checks.  It is not a safety
    # primitive by itself.
    return Path(os.path.realpath(os.path.expanduser(str(path_like))))


def _env_key(key: str, windows: bool | None = None) -> str:
    return _windows_casefold(str(key), windows)


def _env_get(env: dict, key: str, windows: bool | None = None) -> str | None:
    wanted = _env_key(key, windows)
    for candidate, value in env.items():
        if _env_key(candidate, windows) == wanted:
            return str(value)
    return None


def forbidden_profile_roots(env: dict, home: Path | None) -> list[tuple[str, Path]]:
    """Known live/default TraceDecay profile locations and their aliases.

    Kept in sync with src/config.rs (`user_data_dir`): the live profile is
    ``~/.tracedecay`` unless ``TRACEDECAY_DATA_DIR`` overrides it, and the
    global DB lives beside it unless ``TRACEDECAY_GLOBAL_DB`` pins a path.
    """
    roots: list[tuple[str, Path]] = []
    data_dir = _env_get(env, "TRACEDECAY_DATA_DIR")
    if data_dir:
        roots.append(("TRACEDECAY_DATA_DIR", _real(data_dir)))
    global_db = _env_get(env, "TRACEDECAY_GLOBAL_DB")
    if global_db:
        roots.append(("TRACEDECAY_GLOBAL_DB parent", _real(global_db).parent))
    if home is not None:
        roots.append(("default profile ~/.tracedecay", _real(home / ".tracedecay")))
    return roots


def guard_path(candidate, role: str, forbidden: list[tuple[str, Path]]) -> Path:
    """Reject live-profile overlap after lstat-safe lexical normalization."""
    safe = assert_safe_path_components(candidate, role, allow_missing=True)
    real = _real(safe)
    for label, root in forbidden:
        root_real = _real(root)
        same = _normalized_path(real) == _normalized_path(root_real)
        if not same:
            try:
                same = os.path.samefile(real, root)
            except OSError:
                same = False
        if same:
            raise SafetyError(
                f"{role} path {real} resolves to the live/default TraceDecay "
                f"profile location ({label}: {root}); refusing to proceed"
            )
        if _path_is_within(real, root_real):
            raise SafetyError(
                f"{role} path {real} is inside the live/default TraceDecay "
                f"profile location ({label}: {root}); refusing to proceed"
            )
        if _path_is_within(root_real, real):
            raise SafetyError(
                f"{role} path {real} contains the live/default TraceDecay "
                f"profile location ({label}: {root}); refusing to proceed"
            )
    return safe


def require_disjoint_roots(input_root: Path, output_root: Path) -> None:
    """Input and output must not be equal, nested, or aliases of one another."""
    if _path_is_within(input_root, output_root) or _path_is_within(output_root, input_root):
        raise SafetyError(
            "input and output roots must be disjoint; neither may contain the other"
        )
    try:
        if os.path.samefile(input_root, output_root):
            raise SafetyError("input and output roots alias the same filesystem object")
    except FileNotFoundError:
        pass
    except OSError:
        # One root may not exist yet.  The lexical containment check above is
        # decisive because both roots have been checked for links/reparse points.
        pass


def prepare_output_dir(candidate, forbidden: list[tuple[str, Path]]) -> Path:
    output = guard_path(candidate, "output", forbidden)
    # An existing-but-empty directory is not enough: a competing process can
    # replace it after the emptiness check.  The final directory must be ours.
    return create_fresh_directory(output, "output")


def _mount_unescape(value: str) -> str:
    return (
        value.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
    )


def _linux_mounts() -> list[tuple[Path, str]]:
    """Best-effort local mount table used only when Linux exposes mountinfo."""
    mountinfo = Path("/proc/self/mountinfo")
    try:
        lines = mountinfo.read_text(encoding="utf-8").splitlines()
    except OSError:
        return []
    mounts: list[tuple[Path, str]] = []
    for line in lines:
        if " - " not in line:
            continue
        before, after = line.split(" - ", 1)
        fields = before.split()
        fs_fields = after.split()
        if len(fields) < 5 or not fs_fields:
            continue
        mounts.append((_absolute(_mount_unescape(fields[4])), fs_fields[0].lower()))
    return mounts


def filesystem_safety(path_like: str | Path) -> dict[str, str]:
    """Identify network filesystems where stdlib/platform evidence permits it."""
    path = _absolute(path_like)
    if os.name == "nt":
        try:
            import ctypes

            # GetDriveTypeW requires a volume root, not an arbitrary path.
            get_drive_type = ctypes.windll.kernel32.GetDriveTypeW
            get_drive_type.argtypes = [ctypes.c_wchar_p]
            get_drive_type.restype = ctypes.c_uint
            drive_type = get_drive_type(path.anchor)
            if drive_type == 4:
                return {"state": "network", "filesystem_type": "windows_remote_drive"}
            if drive_type in {2, 3, 5, 6}:
                return {"state": "local", "filesystem_type": "windows_drive"}
        except (AttributeError, OSError):
            pass
        return {"state": "not_detectable", "filesystem_type": "unknown"}
    if sys.platform.startswith("linux"):
        matches = [
            (mount, fs_type)
            for mount, fs_type in _linux_mounts()
            if _path_is_within(path, mount)
        ]
        if matches:
            mount, fs_type = max(matches, key=lambda item: len(str(item[0])))
            del mount
            if fs_type.startswith("fuse") and fs_type not in NETWORK_FILESYSTEM_TYPES:
                return {"state": "not_detectable", "filesystem_type": fs_type}
            return {
                "state": "network" if fs_type in NETWORK_FILESYSTEM_TYPES else "local",
                "filesystem_type": fs_type,
            }
    return {"state": "not_detectable", "filesystem_type": "unknown"}


def reject_network_filesystem(path_like: str | Path, role: str) -> dict[str, str]:
    safety = filesystem_safety(path_like)
    if safety["state"] == "network":
        raise SafetyError(
            f"{role} is on detected network filesystem {safety['filesystem_type']}; refusing"
        )
    return safety


def normalized_platform_name(value: str | None = None) -> str:
    raw = (value or platform.system()).strip().lower().replace("_", "-")
    aliases = {
        "darwin": "macos",
        "macos": "macos",
        "mac-os": "macos",
        "win32": "windows",
        "windows": "windows",
        "linux": "linux",
    }
    return aliases.get(raw, raw)


def _scrubbed_env(base_env: dict, windows: bool | None = None) -> dict[str, str]:
    """Drop profile discovery keys with Windows' case-insensitive semantics."""
    result: dict[str, str] = {}
    exact = {_env_key(key, windows) for key in SCRUB_ENV_EXACT}
    prefixes = tuple(_env_key(prefix, windows) for prefix in SCRUB_ENV_PREFIXES)
    protected = {_env_key(key, windows) for key in PROTECTED_CHILD_ENV_KEYS}
    for key, value in base_env.items():
        normalized = _env_key(str(key), windows)
        if normalized in exact or normalized in protected or normalized.startswith(prefixes):
            continue
        result[str(key)] = str(value)
    return result


def _set_env(env: dict[str, str], key: str, value: str, windows: bool | None = None) -> None:
    normalized = _env_key(key, windows)
    for existing in [candidate for candidate in env if _env_key(candidate, windows) == normalized]:
        del env[existing]
    env[key] = value


def create_child_sandbox(
    run_dir: Path, role: str = "child", data_root: Path | None = None
) -> dict[str, Path]:
    """Create all child-discovery roots beneath one runner-owned run directory."""
    sandbox = create_fresh_directory(run_dir / "sandbox", f"{role} sandbox")
    roots: dict[str, Path] = {
        "sandbox": sandbox,
        "home": create_fresh_directory(sandbox / "home", f"{role} home"),
        "config": create_fresh_directory(sandbox / "config", f"{role} config"),
        "cache": create_fresh_directory(sandbox / "cache", f"{role} cache"),
        "runtime": create_fresh_directory(sandbox / "runtime", f"{role} runtime"),
        "cwd": create_fresh_directory(sandbox / "cwd", f"{role} cwd"),
        "output": create_fresh_directory(sandbox / "output", f"{role} output"),
        "temp": create_fresh_directory(sandbox / "temp", f"{role} temp"),
    }
    if data_root is None:
        roots["data"] = create_fresh_directory(sandbox / "tracedecay-data", f"{role} data")
    else:
        roots["data"] = assert_safe_path_components(
            data_root, f"{role} data", require_directory=True
        )
    return roots


def build_child_env(
    base_env: dict,
    declared_env: dict,
    declared_env_path_keys: list[str],
    forbidden: list[tuple[str, Path]],
    sandbox: dict[str, Path] | None = None,
    *,
    windows: bool | None = None,
) -> dict:
    """Build an isolated child environment with case-safe Windows scrubbing."""
    env = _scrubbed_env(base_env, windows)
    protected = {_env_key(key, windows) for key in PROTECTED_CHILD_ENV_KEYS}
    for key in declared_env_path_keys:
        value = _env_get(declared_env, key, windows)
        if value is not None:
            guard_path(value, f"declared env {key}", forbidden)
            if sandbox is not None:
                allowed_roots = [
                    sandbox["home"],
                    sandbox["config"],
                    sandbox["cache"],
                    sandbox["runtime"],
                    sandbox["data"],
                    sandbox["cwd"],
                    sandbox["output"],
                    sandbox["temp"],
                ]
                if not any(_path_is_within(value, root) for root in allowed_roots):
                    raise SafetyError(
                        f"declared env {key} must stay inside a runner-owned child root"
                    )
    for key, value in declared_env.items():
        normalized = _env_key(str(key), windows)
        if normalized in protected or normalized.startswith(_env_key("TRACEDECAY_", windows)):
            raise ConfigError(
                f"declared environment may not override runner-isolated root {key!r}"
            )
        _set_env(env, str(key), str(value), windows)
    if sandbox is not None:
        data_root = sandbox["data"]
        fixed = {
            "HOME": str(sandbox["home"]),
            "USERPROFILE": str(sandbox["home"]),
            "APPDATA": str(sandbox["config"]),
            "LOCALAPPDATA": str(sandbox["config"]),
            "XDG_CONFIG_HOME": str(sandbox["config"]),
            "XDG_CACHE_HOME": str(sandbox["cache"]),
            "XDG_DATA_HOME": str(sandbox["data"]),
            "XDG_STATE_HOME": str(sandbox["cache"]),
            "XDG_RUNTIME_DIR": str(sandbox["runtime"]),
            "TRACEDECAY_DATA_DIR": str(data_root),
            "TRACEDECAY_GLOBAL_DB": str(data_root / "global.db"),
            "TRACEDECAY_CONFIG_DIR": str(sandbox["config"]),
            "TMPDIR": str(sandbox["temp"]),
            "TEMP": str(sandbox["temp"]),
            "TMP": str(sandbox["temp"]),
            "SQLITE_TMPDIR": str(sandbox["temp"]),
            "PWD": str(sandbox["cwd"]),
        }
        for key, value in fixed.items():
            _set_env(env, key, value, windows)
    return env
