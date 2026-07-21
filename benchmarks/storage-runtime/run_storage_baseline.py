#!/usr/bin/env python3
"""TraceDecay SQLite storage runtime — Phase S0 frozen evidence/baseline harness.

This runner is the S0 delivery barrier for the SQLite storage runtime plan. It:

* freezes the released binary, schema, workload, corpus, and config identity
  (`freeze` subcommand),
* defines and executes current, 10x, open-loop overload, crash, recovery, FTS,
  backup/restore, and A/A noise-floor workloads across supported store
  families (`run` subcommand),
* captures environment/toolchain/platform identity, offered/admitted/
  completed/shed/retried counts, latency percentiles measured from the
  scheduled issue time (no coordinated omission), and integrity/count/digest/
   FTS/backup logical evidence into a machine-readable result artifact,
* and never discovers or touches the live TraceDecay profile implicitly.

Safety contract (fail closed):

* The store input directory and the output directory must be supplied
  explicitly. There is no default.
* If either resolves to, is contained in, or contains a known live/default
  profile location (``$TRACEDECAY_DATA_DIR``, the parent of
  ``$TRACEDECAY_GLOBAL_DB``, or ``~/.tracedecay``), including through symlink
  or hardlink aliases, the runner refuses before executing anything.
* Child commands receive runner-owned HOME/config/cache/TraceDecay roots and
  CWD; inherited ``TRACEDECAY_*`` / ``NEXTEST_TEST_NAME`` values are scrubbed
  and workloads cannot override protected roots.
* The output directory must not exist; the runner atomically creates and owns it.
* Workload steps whose command is pending (``null`` argv) refuse to execute
  unless ``--allow-pending`` records them as not run.

Stdlib only. Python 3.10+.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import re
import shutil
import signal
import sqlite3
import stat
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path
from typing import Any

RUNNER_VERSION = "2.0.0"
RESULT_ARTIFACT_ID = "storage-runtime-baseline-result-v2"
IDENTITY_ARTIFACT_ID = "storage-runtime-frozen-identity-v2"
WORKLOAD_SCHEMA_VERSION = 1
RESULT_SCHEMA_VERSION = 2
IDENTITY_SCHEMA_VERSION = 2
LOGICAL_SQLITE_EVIDENCE_SCHEMA = "storage-runtime-logical-sqlite-evidence-v1"

SCRUB_ENV_PREFIXES = ("TRACEDECAY_",)
SCRUB_ENV_EXACT = ("NEXTEST_TEST_NAME",)
PROTECTED_CHILD_ENV_KEYS = {
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "PWD",
    "OLDPWD",
    "TRACEDECAY_DATA_DIR",
    "TRACEDECAY_GLOBAL_DB",
    "TRACEDECAY_CONFIG_DIR",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SQLITE_TMPDIR",
}
SAFE_IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")
SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")
NETWORK_FILESYSTEM_TYPES = {
    "9p",
    "afs",
    "cifs",
    "ceph",
    "fuse.sshfs",
    "glusterfs",
    "nfs",
    "nfs4",
    "lustre",
    "gpfs",
    "panfs",
    "orangefs",
    "smbfs",
    "smb3",
    "sshfs",
    "davfs",
    "fuse.davfs",
    "fuse.gcsfuse",
    "fuse.rclone",
    "fuse.s3fs",
}

DEFAULT_OUTCOME_MAP = {"0": "completed"}


class RunnerError(Exception):
    """Base error for expected runner failures."""


class SafetyError(RunnerError):
    """Raised when a path or environment would touch a live profile."""


class ConfigError(RunnerError):
    """Raised for invalid or pending workload configuration."""


class ExecutionError(RunnerError):
    """Raised when a workload step fails in a way that aborts the run."""


# ---------------------------------------------------------------------------
# Path safety, hashing, and fingerprints
# ---------------------------------------------------------------------------


def _windows_casefold(value: str, windows: bool | None = None) -> str:
    """Normalize filesystem/environment comparisons on Windows only."""
    if windows is None:
        windows = os.name == "nt"
    return value.casefold() if windows else value


def _absolute(path_like: str | Path) -> Path:
    return Path(os.path.abspath(os.path.expanduser(str(path_like))))


def _normalized_path(path_like: str | Path, windows: bool | None = None) -> str:
    return _windows_casefold(os.path.normpath(str(_absolute(path_like))), windows)


def _path_is_within(child: str | Path, parent: str | Path) -> bool:
    """Lexical containment after absolute normalization, never symlink resolution."""
    child_text = _normalized_path(child)
    parent_text = _normalized_path(parent)
    try:
        return os.path.commonpath([child_text, parent_text]) == parent_text
    except ValueError:
        return False


def _is_reparse_point(info: os.stat_result) -> bool:
    """Return whether a Windows lstat result is a reparse point/junction."""
    attributes = getattr(info, "st_file_attributes", 0)
    reparse = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & reparse)


def _node_kind(info: os.stat_result) -> str:
    mode = info.st_mode
    if stat.S_ISLNK(mode):
        return "symlink"
    if _is_reparse_point(info):
        return "reparse point"
    if stat.S_ISDIR(mode):
        return "directory"
    if stat.S_ISREG(mode):
        return "regular file"
    if stat.S_ISCHR(mode):
        return "character device"
    if stat.S_ISBLK(mode):
        return "block device"
    if stat.S_ISFIFO(mode):
        return "fifo"
    if stat.S_ISSOCK(mode):
        return "socket"
    return "unknown special file"


def _reject_unsafe_node(path: Path, info: os.stat_result, role: str) -> None:
    kind = _node_kind(info)
    if kind not in {"directory", "regular file"}:
        raise SafetyError(f"{role} contains unsafe {kind}: {path}")
    # A regular input file with more than one directory entry can alias an
    # operator-owned store outside the declared corpus.  A runner-created copy
    # is always nlink==1 after publication, so reject all others fail closed.
    if kind == "regular file" and getattr(info, "st_nlink", 1) != 1:
        raise SafetyError(f"{role} contains unsafe hardlinked file: {path}")


def assert_safe_path_components(
    path_like: str | Path,
    role: str,
    *,
    allow_missing: bool = False,
    require_directory: bool | None = None,
) -> Path:
    """lstat every existing component and reject links/reparse/special files.

    ``Path.resolve`` is deliberately not used: resolving a supplied path would
    traverse the very symlink/junction this harness is required to reject.
    Missing suffixes are accepted only for runner-created destinations.
    """
    path = _absolute(path_like)
    anchor = Path(path.anchor)
    if not path.anchor:
        raise SafetyError(f"{role} path has no filesystem anchor: {path}")
    current = anchor
    try:
        root_info = os.lstat(current)
    except OSError as exc:
        raise SafetyError(f"cannot lstat filesystem root for {role}: {exc}") from exc
    _reject_unsafe_node(current, root_info, role)
    if not stat.S_ISDIR(root_info.st_mode):  # defensive; an anchor must be a dir
        raise SafetyError(f"filesystem root for {role} is not a directory")

    parts = path.parts[1:]
    for index, part in enumerate(parts):
        current = current / part
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            if allow_missing:
                break
            raise SafetyError(f"{role} path does not exist: {path}")
        except OSError as exc:
            raise SafetyError(f"cannot lstat {role} path {current}: {exc}") from exc
        _reject_unsafe_node(current, info, role)
        if index < len(parts) - 1 and not stat.S_ISDIR(info.st_mode):
            raise SafetyError(f"{role} path has non-directory ancestor: {current}")
        if index == len(parts) - 1 and require_directory is True and not stat.S_ISDIR(
            info.st_mode
        ):
            raise SafetyError(f"{role} path is not a directory: {path}")
        if index == len(parts) - 1 and require_directory is False and not stat.S_ISREG(
            info.st_mode
        ):
            raise SafetyError(f"{role} path is not a regular file: {path}")
    return path


def validate_safe_tree(root_like: str | Path, role: str) -> Path:
    """Recursively lstat a tree before it becomes a benchmark input.

    os.walk's default non-following mode is insufficient because it silently
    skips links and accepts special nodes.  The explicit lstat recursion makes
    every accepted object an ordinary directory or a single-link regular file.
    """
    root = assert_safe_path_components(root_like, role, require_directory=True)

    def visit(directory: Path) -> None:
        assert_safe_path_components(directory, role, require_directory=True)
        before = os.lstat(directory)
        try:
            with os.scandir(directory) as iterator:
                entries = sorted(iterator, key=lambda entry: entry.name)
        except OSError as exc:
            raise SafetyError(f"cannot scan {role} directory {directory}: {exc}") from exc
        try:
            after = os.lstat(directory)
        except OSError as exc:
            raise SafetyError(f"cannot recheck {role} directory {directory}: {exc}") from exc
        _reject_unsafe_node(directory, after, role)
        if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
            raise SafetyError(f"{role} directory changed while scanning: {directory}")
        for entry in entries:
            child = directory / entry.name
            try:
                info = os.lstat(child)
            except OSError as exc:
                raise SafetyError(f"cannot lstat {role} entry {child}: {exc}") from exc
            _reject_unsafe_node(child, info, role)
            if stat.S_ISDIR(info.st_mode):
                visit(child)

    visit(root)
    return root


def _open_read_no_follow(path: Path, role: str) -> int:
    """Open one validated regular file and re-check identity after open."""
    assert_safe_path_components(path, role, require_directory=False)
    before = os.lstat(path)
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise SafetyError(f"cannot safely open {role} file {path}: {exc}") from exc
    try:
        after = os.fstat(descriptor)
        if not stat.S_ISREG(after.st_mode) or getattr(after, "st_nlink", 1) != 1:
            raise SafetyError(f"{role} file changed to an unsafe type while opening: {path}")
        if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
            raise SafetyError(f"{role} file changed while opening: {path}")
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def sha256_file(path: Path, role: str = "hashed") -> str:
    digest = hashlib.sha256()
    descriptor = _open_read_no_follow(path, role)
    with os.fdopen(descriptor, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def fingerprint_tree(root: Path, role: str = "corpus") -> dict:
    """Content identity of a directory tree, path-independent.

    Returns sorted (relative path, sha256) entries plus one aggregate digest.
    """
    root = validate_safe_tree(root, role)
    entries = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        for name in sorted(filenames):
            full = Path(dirpath) / name
            rel = full.relative_to(root).as_posix()
            entries.append({"path": rel, "sha256": sha256_file(full, role)})
    # Canonical JSON avoids delimiter ambiguity for valid filenames containing
    # whitespace or control characters.
    aggregate = sha256_text(
        json.dumps(entries, separators=(",", ":"), ensure_ascii=False)
    )
    return {"file_count": len(entries), "aggregate_sha256": aggregate, "files": entries}


def artifact_fingerprint(path_like: str | Path, role: str) -> dict[str, Any]:
    """Hash an explicitly supplied immutable file or safe corpus/config tree."""
    path = assert_safe_path_components(path_like, role)
    info = os.lstat(path)
    if stat.S_ISREG(info.st_mode):
        return {
            "kind": "file",
            "basename": path.name,
            "sha256": sha256_file(path, role),
            "size_bytes": info.st_size,
        }
    if stat.S_ISDIR(info.st_mode):
        tree = fingerprint_tree(path, role)
        return {
            "kind": "tree",
            "basename": path.name,
            "file_count": tree["file_count"],
            "aggregate_sha256": tree["aggregate_sha256"],
        }
    raise SafetyError(f"{role} is neither a regular file nor a directory: {path}")


def _safe_mkdir_parents(path: Path, role: str) -> None:
    """Create missing parent directories one lstat-checked component at a time."""
    path = _absolute(path)
    anchor = Path(path.anchor)
    current = anchor
    for part in path.parts[1:]:
        current = current / part
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            try:
                os.mkdir(current, 0o700)
            except FileExistsError:
                # A racing creator is acceptable only if it created a safe dir.
                pass
            except OSError as exc:
                raise SafetyError(f"cannot create {role} parent {current}: {exc}") from exc
            try:
                info = os.lstat(current)
            except OSError as exc:
                raise SafetyError(f"cannot recheck {role} parent {current}: {exc}") from exc
        _reject_unsafe_node(current, info, role)
        if not stat.S_ISDIR(info.st_mode):
            raise SafetyError(f"{role} parent is not a directory: {current}")


def create_fresh_directory(path_like: str | Path, role: str) -> Path:
    """Atomically create one fresh directory without following an existing leaf."""
    path = assert_safe_path_components(path_like, role, allow_missing=True)
    _safe_mkdir_parents(path.parent, role)
    try:
        os.mkdir(path, 0o700)
    except FileExistsError as exc:
        raise SafetyError(f"{role} path already exists; a fresh path is required: {path}") from exc
    except OSError as exc:
        raise SafetyError(f"cannot create fresh {role} directory {path}: {exc}") from exc
    assert_safe_path_components(path, role, require_directory=True)
    return path


def _open_create_new(path: Path, role: str) -> int:
    """Create a file with O_EXCL and O_NOFOLLOW when the platform provides it."""
    assert_safe_path_components(path.parent, role, require_directory=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags, 0o600)
    except FileExistsError as exc:
        raise SafetyError(f"{role} output already exists: {path}") from exc
    except OSError as exc:
        raise SafetyError(f"cannot atomically create {role} output {path}: {exc}") from exc
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or getattr(info, "st_nlink", 1) != 1:
            raise SafetyError(f"{role} output became unsafe while creating: {path}")
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def atomic_write_new(path_like: str | Path, data: str, role: str) -> Path:
    """Publish a no-replace output after fsyncing a private O_EXCL temporary.

    ``os.link`` gives a portable stdlib no-replace publish operation on the
    local filesystems this runner admits.  We intentionally fail closed rather
    than fall back to ``os.replace`` (which can overwrite an attacker-created
    destination on POSIX).
    """
    path = _absolute(path_like)
    assert_safe_path_components(path.parent, role, require_directory=True)
    if not SAFE_IDENTIFIER.fullmatch(path.name):
        raise SafetyError(f"unsafe {role} output filename: {path.name!r}")
    temp = path.parent / f".{path.name}.{uuid.uuid4().hex}.tmp"
    descriptor = _open_create_new(temp, role)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(temp, path, follow_symlinks=False)
        except (AttributeError, NotImplementedError, OSError, TypeError) as exc:
            raise SafetyError(
                f"{role} requires atomic no-replace publication, which is unavailable: {exc}"
            ) from exc
    finally:
        try:
            os.unlink(temp)
        except FileNotFoundError:
            pass
    assert_safe_path_components(path, role, require_directory=False)
    return path


def copy_safe_file(source: Path, destination: Path, role: str) -> None:
    """Copy a single validated regular file into a fresh no-follow destination."""
    source_descriptor = _open_read_no_follow(source, role)
    try:
        destination_descriptor = _open_create_new(destination, role)
    except BaseException:
        os.close(source_descriptor)
        raise
    try:
        with os.fdopen(source_descriptor, "rb") as source_handle, os.fdopen(
            destination_descriptor, "wb"
        ) as destination_handle:
            shutil.copyfileobj(source_handle, destination_handle, length=1 << 20)
            destination_handle.flush()
            os.fsync(destination_handle.fileno())
    except BaseException:
        try:
            destination.unlink()
        except FileNotFoundError:
            pass
        raise


def copy_safe_tree(source_like: str | Path, destination_like: str | Path, role: str) -> Path:
    """Make a fully independent, runner-owned store copy for exactly one run."""
    source = validate_safe_tree(source_like, f"{role} source")
    destination = create_fresh_directory(destination_like, f"{role} destination")

    def copy_directory(source_dir: Path, destination_dir: Path) -> None:
        with os.scandir(source_dir) as iterator:
            entries = sorted(iterator, key=lambda entry: entry.name)
        for entry in entries:
            source_child = source_dir / entry.name
            destination_child = destination_dir / entry.name
            info = os.lstat(source_child)
            _reject_unsafe_node(source_child, info, role)
            if stat.S_ISDIR(info.st_mode):
                child_dir = create_fresh_directory(destination_child, f"{role} copy")
                copy_directory(source_child, child_dir)
            else:
                copy_safe_file(source_child, destination_child, role)

    copy_directory(source, destination)
    validate_safe_tree(destination, f"{role} copied store")
    return destination


def copy_safe_artifact(source_like: str | Path, destination_dir: Path, role: str) -> Path:
    """Copy a frozen config artifact beneath a runner-owned config root."""
    source = assert_safe_path_components(source_like, role)
    assert_safe_path_components(destination_dir, role, require_directory=True)
    info = os.lstat(source)
    if stat.S_ISDIR(info.st_mode):
        return copy_safe_tree(source, destination_dir / "frozen-config", role)
    if stat.S_ISREG(info.st_mode):
        destination = destination_dir / source.name
        copy_safe_file(source, destination, role)
        return destination
    raise SafetyError(f"{role} is neither a regular file nor a directory")


# ---------------------------------------------------------------------------
# Live-profile guard
# ---------------------------------------------------------------------------


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


# ---------------------------------------------------------------------------
# Latency statistics (nearest-rank, matching existing repo benchmark style)
# ---------------------------------------------------------------------------


def nearest_rank(sorted_samples: list[int], percentile: float) -> int | None:
    if not sorted_samples:
        return None
    rank = max(1, math.ceil(percentile / 100.0 * len(sorted_samples)))
    return sorted_samples[rank - 1]


def summarize_latency(samples_ns: list[int]) -> dict:
    ordered = sorted(samples_ns)
    return {
        "count": len(ordered),
        "min_ns": ordered[0] if ordered else None,
        "p50_ns": nearest_rank(ordered, 50),
        "p95_ns": nearest_rank(ordered, 95),
        "p99_ns": nearest_rank(ordered, 99),
        "max_ns": ordered[-1] if ordered else None,
        "sample_stddev_ns": (
            statistics.stdev(ordered) if len(ordered) >= 2 else 0.0 if ordered else None
        ),
        "percentile_method": "nearest_rank",
    }


# ---------------------------------------------------------------------------
# Counts
# ---------------------------------------------------------------------------


def new_counts() -> dict:
    return {
        "offered": 0,
        "admitted": 0,
        "completed": 0,
        "failed": 0,
        "retried": 0,
        "shed": {"runner_in_flight_cap": 0, "command_saturation": 0},
    }


def counts_invariants_ok(counts: dict) -> list[str]:
    """Return a list of violated invariants (empty when consistent)."""
    if not isinstance(counts, dict):
        return ["counts must be an object"]
    problems: list[str] = []
    scalar_keys = ("offered", "admitted", "completed", "failed", "retried")
    for key in scalar_keys:
        if not isinstance(counts.get(key), int) or isinstance(counts.get(key), bool):
            problems.append(f"{key} must be an integer")
    shed = counts.get("shed")
    if not isinstance(shed, dict):
        problems.append("shed must be an object")
    else:
        for key in ("runner_in_flight_cap", "command_saturation"):
            if not isinstance(shed.get(key), int) or isinstance(shed.get(key), bool):
                problems.append(f"shed.{key} must be an integer")
    if problems:
        return problems
    shed_runner = counts["shed"]["runner_in_flight_cap"]
    shed_command = counts["shed"]["command_saturation"]
    if counts["offered"] != counts["admitted"] + shed_runner:
        problems.append("offered != admitted + shed.runner_in_flight_cap")
    if counts["admitted"] != counts["completed"] + counts["failed"] + shed_command:
        problems.append("admitted != completed + failed + shed.command_saturation")
    for key in scalar_keys:
        if counts[key] < 0:
            problems.append(f"{key} is negative")
    for key in ("runner_in_flight_cap", "command_saturation"):
        value = counts["shed"][key]
        if value < 0:
            problems.append(f"shed.{key} is negative")
    return problems


# ---------------------------------------------------------------------------
# Command execution and placeholder substitution
# ---------------------------------------------------------------------------


PATH_PLACEHOLDERS = {
    "BINARY",
    "INPUT",
    "OUTPUT",
    "RUN_DIR",
    "HOME",
    "CONFIG",
    "CACHE",
    "TRACEDECAY_DATA_DIR",
    "TRACEDECAY_GLOBAL_DB",
}


def require_safe_identifier(value: object, role: str) -> str:
    if not isinstance(value, str):
        raise ConfigError(f"{role} must be a string safe identifier")
    text = value
    stem = text.split(".", 1)[0].casefold()
    windows_reserved = {"con", "prn", "aux", "nul", "clock$"} | {
        f"{prefix}{number}" for prefix in ("com", "lpt") for number in range(1, 10)
    }
    if (
        text in {".", ".."}
        or text.endswith(".")
        or stem in windows_reserved
        or not SAFE_IDENTIFIER.fullmatch(text)
    ):
        raise ConfigError(
            f"{role} must be a safe identifier ([A-Za-z0-9][A-Za-z0-9_.-]{{0,63}})"
        )
    return text


def substitute(arg: str, mapping: dict[str, str]) -> str:
    for token, value in mapping.items():
        arg = arg.replace(f"__{token}__", value)
    if re.search(r"__[A-Z0-9_]+__", arg):
        raise ConfigError(f"unresolved command placeholder in argument")
    return arg


def safe_expanded_path(
    value: str,
    root: Path,
    role: str,
    *,
    allow_missing: bool = True,
    require_directory: bool | None = None,
) -> Path:
    """Require an expanded path to remain inside its runner-owned root."""
    candidate = _absolute(value)
    if not _path_is_within(candidate, root):
        raise SafetyError(f"{role} escapes its allowed runner-owned root")
    assert_safe_path_components(root, role, require_directory=True)
    return assert_safe_path_components(
        candidate, role, allow_missing=allow_missing, require_directory=require_directory
    )


def _path_portion(argument: str) -> str:
    """Extract an option's value for --flag=/path style argv entries."""
    if argument.startswith("-") and "=" in argument:
        return argument.split("=", 1)[1]
    return argument


def substitute_argv(
    argv: list[str], mapping: dict[str, str], path_roots: dict[str, Path] | None = None
) -> list[str]:
    expanded: list[str] = []
    for raw_arg in argv:
        template = str(raw_arg)
        value = substitute(template, mapping)
        if "__PYTHON__" in template and _path_portion(value) != mapping.get("PYTHON"):
            raise SafetyError("expanded Python executable path may not be modified by a template")
        if path_roots is not None:
            for token in PATH_PLACEHOLDERS:
                marker = f"__{token}__"
                if marker in template:
                    root = path_roots.get(token)
                    if root is None:
                        raise ConfigError(f"no containment root declared for {marker}")
                    if token == "BINARY":
                        if _path_portion(value) != mapping.get("BINARY"):
                            raise SafetyError("expanded binary path may not be modified by a template")
                        assert_safe_path_components(
                            Path(mapping["BINARY"]), "expanded binary", require_directory=False
                        )
                        continue
                    safe_expanded_path(
                        _path_portion(value), root, f"expanded command argument {token}"
                    )
        expanded.append(value)
    if not expanded:
        raise ConfigError("command argv must not be empty")
    return expanded


def process_tree_capability(platform_name: str | None = None) -> dict[str, str]:
    """Describe exactly what stdlib can guarantee for descendant cleanup."""
    target = normalized_platform_name(platform_name) if platform_name else None
    if (target == "windows") or (target is None and os.name == "nt"):
        return {
            "state": "unsupported_no_safe_stdlib_job_object",
            "mechanism": "root_process_only",
            "descendant_verification": "unsupported",
        }
    if (target in {None, "linux", "macos"}) and os.name == "posix":
        return {
            "state": "supported_best_effort",
            "mechanism": "new_process_group",
            "descendant_verification": "process_group_liveness",
        }
    return {
        "state": "unsupported_platform",
        "mechanism": "none",
        "descendant_verification": "unsupported",
    }


def _popen_group_kwargs() -> dict[str, Any]:
    if os.name == "posix":
        return {"start_new_session": True}
    if os.name == "nt":
        return {"creationflags": getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0x00000200)}
    return {}


def _posix_group_alive(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # Existence without permission cannot be safely verified as gone.
        return True
    return True


def _posix_group_has_live_members(process_group: int) -> bool:
    """Best-effort distinguish a live descendant from an unreapable zombie.

    A killed grandchild can briefly (or on a broken PID 1, persistently) remain
    as a zombie after its parent is killed.  It cannot execute or mutate a
    store, so Linux /proc lets us verify that no *live* descendant remains.
    Other POSIX hosts retain the conservative process-group liveness fallback.
    """
    proc_root = Path("/proc")
    if not proc_root.is_dir():
        return _posix_group_alive(process_group)
    seen = False
    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return _posix_group_alive(process_group)
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            text = (entry / "stat").read_text(encoding="utf-8")
            tail = text[text.rfind(")") + 1 :].split()
            # tail = state, ppid, pgrp, ...
            if len(tail) < 3 or int(tail[2]) != process_group:
                continue
            seen = True
            if tail[0] != "Z":
                return True
        except (OSError, ValueError):
            continue
    return False if seen else _posix_group_alive(process_group)


def _wait_for_no_live_posix_group(process_group: int, timeout_seconds: float) -> bool:
    deadline = time.monotonic() + timeout_seconds
    while _posix_group_has_live_members(process_group):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.01)
    return True


def terminate_process_tree(proc: subprocess.Popen, *, grace_seconds: float = 0.5) -> dict[str, str]:
    """Terminate a child process group and verify group disappearance on POSIX."""
    capability = process_tree_capability()
    if os.name != "posix":
        if proc.poll() is None:
            proc.kill()
        try:
            proc.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired:
            pass
        return {
            **capability,
            "termination": "root_terminated_descendants_unverifiable",
            "clean": "unsupported",
        }

    process_group = proc.pid
    if _posix_group_alive(process_group):
        try:
            os.killpg(process_group, signal.SIGTERM)
        except ProcessLookupError:
            pass
    try:
        proc.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        pass
    if _posix_group_alive(process_group):
        try:
            os.killpg(process_group, signal.SIGKILL)
        except ProcessLookupError:
            pass
    try:
        proc.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        pass
    clean = _wait_for_no_live_posix_group(process_group, grace_seconds)
    return {
        **capability,
        "termination": "group_terminated" if clean else "group_termination_unverified",
        "clean": "true" if clean else "false",
    }


def kill_process_tree(proc: subprocess.Popen, *, grace_seconds: float = 0.5) -> dict[str, str]:
    """Abruptly kill a POSIX process group for crash-recovery workloads."""
    capability = process_tree_capability()
    if os.name != "posix":
        return terminate_process_tree(proc, grace_seconds=grace_seconds)
    process_group = proc.pid
    try:
        os.killpg(process_group, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        proc.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        pass
    clean = _wait_for_no_live_posix_group(process_group, grace_seconds)
    return {
        **capability,
        "termination": "sigkill_process_group" if clean else "sigkill_unverified",
        "clean": "true" if clean else "false",
    }


def _redacted_stream_summary(stream) -> dict[str, Any]:
    stream.flush()
    stream.seek(0)
    digest = hashlib.sha256()
    byte_count = 0
    newline_count = 0
    for chunk in iter(lambda: stream.read(1 << 20), b""):
        digest.update(chunk)
        byte_count += len(chunk)
        newline_count += chunk.count(b"\n")
    return {
        "redacted": True,
        "sha256": digest.hexdigest(),
        "byte_count": byte_count,
        "line_count": newline_count + (1 if byte_count else 0),
    }


def command_failure_detail(result: dict) -> str:
    """Diagnostics with hashes only: never echo command stdout/stderr evidence."""
    return (
        f"exit={result.get('exit_code')} timed_out={result.get('timed_out')} "
        f"stdout={result.get('stdout')} stderr={result.get('stderr')}"
    )


def preferred_output_summary(result: dict) -> dict[str, Any]:
    return result["stdout"] if result["stdout"]["byte_count"] else result["stderr"]


def run_command(
    argv: list[str],
    env: dict,
    timeout_seconds: float,
    cwd: Path | None = None,
) -> dict:
    if not argv:
        raise ConfigError("command argv must not be empty")
    if cwd is not None:
        assert_safe_path_components(cwd, "child cwd", require_directory=True)
    started = time.monotonic_ns()
    temp_dir = str(cwd) if cwd else None
    with tempfile.TemporaryFile(dir=temp_dir) as stdout_file, tempfile.TemporaryFile(
        dir=temp_dir
    ) as stderr_file:
        try:
            proc = subprocess.Popen(
                argv,
                env=env,
                cwd=str(cwd) if cwd else None,
                stdout=stdout_file,
                stderr=stderr_file,
                **_popen_group_kwargs(),
            )
        except OSError as exc:
            raise ExecutionError(
                f"failed to execute command {Path(argv[0]).name!r}: {type(exc).__name__}"
            ) from exc

        timed_out = False
        try:
            proc.communicate(timeout=timeout_seconds)
            if os.name == "posix":
                process_tree = process_tree_capability()
                if _posix_group_has_live_members(proc.pid):
                    # A normal parent exit with a surviving descendant is still a
                    # leak. Kill it before returning and invalidate the request.
                    process_tree = terminate_process_tree(proc)
                    process_tree["termination"] = "descendant_leak_terminated"
                    process_tree["clean"] = "false"
                else:
                    process_tree.update({"termination": "not_required", "clean": "true"})
            else:
                process_tree = {
                    **process_tree_capability(),
                    "termination": "not_required",
                    "clean": "unsupported",
                }
        except subprocess.TimeoutExpired:
            timed_out = True
            process_tree = terminate_process_tree(proc)
            try:
                proc.communicate(timeout=0.5)
            except subprocess.TimeoutExpired:
                process_tree["clean"] = "false"
                process_tree["termination"] = "group_termination_unverified"
        finished = time.monotonic_ns()
        return {
            "exit_code": proc.returncode,
            "wall_ns": finished - started,
            "stdout": _redacted_stream_summary(stdout_file),
            "stderr": _redacted_stream_summary(stderr_file),
            "timed_out": timed_out,
            "process_tree": process_tree,
        }


def map_outcome(exit_code, timed_out: bool, outcome_map: dict) -> str:
    if timed_out:
        return "failed"
    mapped = outcome_map.get(str(exit_code))
    if mapped is not None:
        return mapped
    return "failed"


def command_succeeded(result: dict, expected_exit_code: int = 0) -> bool:
    """A leaked POSIX process group invalidates an otherwise-zero command."""
    return (
        not result["timed_out"]
        and result["exit_code"] == expected_exit_code
        and result.get("process_tree", {}).get("clean") != "false"
    )


# ---------------------------------------------------------------------------
# Environment / identity capture (no machine-specific absolute paths)
# ---------------------------------------------------------------------------


def safe_probe_base_env(base_env: dict) -> dict[str, str]:
    """Minimal inherited environment for version probes, not the operator profile."""
    allowed = {
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "COMSPEC",
        "LANG",
        "LC_ALL",
        "TZ",
        "TMP",
        "TEMP",
    }
    normalized_allowed = {_env_key(item) for item in allowed}
    return {
        key: str(value)
        for key, value in base_env.items()
        if _env_key(key) in normalized_allowed
    }


def capture_environment(
    workload: dict,
    host_label: str | None,
    record_hostname: bool,
    probe_root: Path,
    forbidden: list[tuple[str, Path]],
) -> dict:
    probe_sandbox = create_child_sandbox(probe_root, "version probe")
    probe_env = build_child_env(
        safe_probe_base_env(dict(os.environ)), {}, [], forbidden, probe_sandbox
    )
    path_roots = {
        "HOME": probe_sandbox["home"],
        "CONFIG": probe_sandbox["config"],
        "CACHE": probe_sandbox["cache"],
        "TRACEDECAY_DATA_DIR": probe_sandbox["data"],
        "TRACEDECAY_GLOBAL_DB": probe_sandbox["data"],
    }
    env_block = {
        "os": normalized_platform_name(),
        "platform_id": normalized_platform_name(),
        "os_release": platform.release(),
        "os_version": platform.version(),
        "machine": platform.machine(),
        "python_version": platform.python_version(),
        "cpu_count_logical": os.cpu_count(),
        "hostname": platform.node() if record_hostname else "redacted",
        "host_label": host_label or "unspecified",
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "runner_version": RUNNER_VERSION,
        "tool_versions": {},
    }
    for name, argv in (workload.get("environment", {}).get("version_commands") or {}).items():
        require_safe_identifier(name, "version command name")
        try:
            command = substitute_argv(
                [str(a) for a in argv], {"PYTHON": sys.executable}, path_roots
            )
            result = run_command(command, probe_env, 15.0, cwd=probe_sandbox["cwd"])
            env_block["tool_versions"][name] = {
                "status": "available" if command_succeeded(result) else "unavailable",
                "exit_code": result["exit_code"],
                "output": preferred_output_summary(result),
                "process_tree": result["process_tree"],
            }
        except RunnerError:
            env_block["tool_versions"][name] = {"status": "unavailable"}
    validate_safe_tree(probe_root, "version probe output")
    return env_block


def binary_identity(binary_path) -> dict:
    path = assert_safe_path_components(binary_path, "binary", require_directory=False)
    info = os.lstat(path)
    return {
        "basename": path.name,
        "sha256": sha256_file(path, "binary"),
        "size_bytes": info.st_size,
    }


# ---------------------------------------------------------------------------
# Workload loading and validation
# ---------------------------------------------------------------------------

REQUIRED_PHASE_KINDS = {
    "closed_loop",
    "open_loop",
    "crash",
    "recovery",
    "backup_restore",
    "aa_pairs",
}


def _require_unique_identifiers(values: object, role: str) -> list[str]:
    if not isinstance(values, list) or not values:
        raise ConfigError(f"{role} must be a non-empty list")
    validated = [require_safe_identifier(value, role) for value in values]
    folded = [value.casefold() for value in validated]
    if len(set(folded)) != len(folded):
        raise ConfigError(f"{role} must be unique, including case-insensitively")
    return validated


def _validate_step(step: object, role: str) -> None:
    if not isinstance(step, dict):
        raise ConfigError(f"{role} must be an object")
    argv = step.get("argv")
    if argv is None:
        return
    if not isinstance(argv, list) or not argv or not all(isinstance(arg, str) for arg in argv):
        raise ConfigError(f"{role} argv must be null or a non-empty string list")


def _config_int(value: object, role: str, minimum: int) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise ConfigError(f"{role} must be an integer >= {minimum}")
    return value


def _config_number(value: object, role: str, minimum: float, *, strict: bool = False) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise ConfigError(f"{role} must be a finite number")
    number = float(value)
    if not math.isfinite(number) or number < minimum or (strict and number == minimum):
        comparator = ">" if strict else ">="
        raise ConfigError(f"{role} must be finite and {comparator} {minimum}")
    return number


def load_workload(path: Path) -> dict:
    path = assert_safe_path_components(path, "workload", require_directory=False)
    try:
        with os.fdopen(_open_read_no_follow(path, "workload"), "r", encoding="utf-8") as handle:
            workload = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise ConfigError(f"cannot load workload {path}: {exc}") from exc
    if not isinstance(workload, dict):
        raise ConfigError(f"workload {path} must contain a JSON object")
    if workload.get("schema_version") != WORKLOAD_SCHEMA_VERSION:
        raise ConfigError(
            f"workload {path} schema_version must be {WORKLOAD_SCHEMA_VERSION}, "
            f"got {workload.get('schema_version')!r}"
        )
    for key in ("workload_id", "store_families", "phases"):
        if key not in workload:
            raise ConfigError(f"workload {path} is missing required key {key!r}")
    require_safe_identifier(workload["workload_id"], "workload_id")
    families = _require_unique_identifiers(workload["store_families"], "store families")
    if not isinstance(workload["phases"], list) or not workload["phases"]:
        raise ConfigError("workload phases must be a non-empty list")
    evidence_eligible = workload.get("evidence_eligible", False)
    if not isinstance(evidence_eligible, bool):
        raise ConfigError("workload evidence_eligible must be boolean")
    workload["evidence_eligible"] = evidence_eligible
    safety = workload.get("safety") or {}
    if not isinstance(safety, dict) or not isinstance(safety.get("env") or {}, dict):
        raise ConfigError("workload safety and safety.env must be objects")
    env_path_keys = safety.get("env_path_keys") or []
    if not isinstance(env_path_keys, list) or not all(
        isinstance(key, str) for key in env_path_keys
    ):
        raise ConfigError("workload safety.env_path_keys must be a string list")
    environment = workload.get("environment") or {}
    if not isinstance(environment, dict):
        raise ConfigError("workload environment must be an object")
    version_commands = environment.get("version_commands") or {}
    if not isinstance(version_commands, dict):
        raise ConfigError("workload environment.version_commands must be an object")
    for name, argv in version_commands.items():
        require_safe_identifier(name, "version command name")
        _validate_step({"argv": argv}, f"version command {name!r}")
    frozen_ref = workload.get("frozen_identity") or {}
    if not isinstance(frozen_ref, dict):
        raise ConfigError("workload frozen_identity must be an object")
    defaults = workload.get("defaults") or {}
    if not isinstance(defaults, dict):
        raise ConfigError("workload defaults must be an object")
    if "warmup" in defaults:
        _config_int(defaults["warmup"], "defaults warmup", 0)
    if "repetitions" in defaults:
        _config_int(defaults["repetitions"], "defaults repetitions", 1)
    if "timeout_seconds" in defaults:
        _config_number(defaults["timeout_seconds"], "defaults timeout_seconds", 0, strict=True)
    seen_phase_names: set[str] = set()
    for phase in workload["phases"]:
        if not isinstance(phase, dict):
            raise ConfigError("each workload phase must be an object")
        name = phase.get("name")
        kind = phase.get("kind")
        name = require_safe_identifier(name, "phase name")
        folded_name = name.casefold()
        if folded_name in seen_phase_names:
            raise ConfigError(f"phase names must be non-empty and unique, got {name!r}")
        seen_phase_names.add(folded_name)
        if kind not in REQUIRED_PHASE_KINDS:
            raise ConfigError(f"phase {name!r} has unknown kind {kind!r}")
        phase_families = _require_unique_identifiers(
            phase.get("families"), f"phase {name!r} families"
        )
        unknown = set(phase_families) - set(families)
        if unknown:
            raise ConfigError(
                f"phase {name!r} references unknown store families {sorted(unknown)}"
            )
        if kind == "recovery" and not phase.get("depends_on"):
            raise ConfigError(f"recovery phase {name!r} must declare depends_on")
        if kind == "recovery":
            require_safe_identifier(phase["depends_on"], f"phase {name!r} dependency")
        if kind == "aa_pairs" and not phase.get("target_phase"):
            raise ConfigError(f"aa_pairs phase {name!r} must declare target_phase")
        if kind == "aa_pairs":
            require_safe_identifier(phase["target_phase"], f"phase {name!r} target")
        for key in ("setup", "work", "recover", "teardown"):
            if key in phase:
                _validate_step(phase[key], f"phase {name!r} {key}")
        steps = phase.get("steps") or []
        if not isinstance(steps, list):
            raise ConfigError(f"phase {name!r} steps must be a list")
        for index, step in enumerate(steps):
            _validate_step(step, f"phase {name!r} step {index}")
            require_safe_identifier(step.get("name", ""), f"phase {name!r} step name")
        evidence_entries: list[dict] = []
        for key in ("evidence", "post_crash_evidence"):
            entries = phase.get(key) or []
            if not isinstance(entries, list):
                raise ConfigError(f"phase {name!r} {key} must be a list")
            evidence_entries.extend(entries)
        evidence_names: set[str] = set()
        for evidence in evidence_entries:
            if not isinstance(evidence, dict):
                raise ConfigError(f"phase {name!r} evidence entries must be objects")
            evidence_name = require_safe_identifier(
                evidence.get("name", ""), f"phase {name!r} evidence name"
            )
            if evidence_name.casefold() in evidence_names:
                raise ConfigError(f"phase {name!r} evidence names must be unique")
            evidence_names.add(evidence_name.casefold())
            if evidence.get("capture") not in {
                "logical_file",
                "sqlite_logical",
                "stdout_redacted",
            }:
                raise ConfigError(
                    f"phase {name!r} evidence has unsupported capture "
                    f"{evidence.get('capture')!r}"
                )
            if evidence_eligible and evidence.get("capture") == "logical_file":
                raise ConfigError("product-evidence workloads may not use logical_file fixtures")
            if evidence.get("capture") == "stdout_redacted":
                _validate_step(evidence, f"phase {name!r} evidence {evidence_name!r}")
        compares = phase.get("compare") or []
        if not isinstance(compares, list):
            raise ConfigError(f"phase {name!r} compare must be a list")
        for comparison in compares:
            if not isinstance(comparison, dict) or not all(
                isinstance(comparison.get(key), str) for key in ("a", "b")
            ):
                raise ConfigError(f"phase {name!r} comparisons need string a/b references")
            if comparison.get("expect", "equal") not in {"equal", "different"}:
                raise ConfigError(f"phase {name!r} comparison has an unknown expectation")
    phase_by_name = {phase["name"]: phase for phase in workload["phases"]}
    phase_positions = {phase["name"]: index for index, phase in enumerate(workload["phases"])}
    for phase in workload["phases"]:
        if phase["kind"] == "recovery":
            dependency = phase_by_name.get(phase["depends_on"])
            if dependency is None or dependency["kind"] != "crash":
                raise ConfigError(f"recovery phase {phase['name']!r} must depend on a crash phase")
            if phase_positions[dependency["name"]] >= phase_positions[phase["name"]]:
                raise ConfigError(f"recovery phase {phase['name']!r} dependency must run first")
        elif phase["kind"] == "aa_pairs":
            target = phase_by_name.get(phase["target_phase"])
            if target is None or target["kind"] != "closed_loop":
                raise ConfigError(f"aa_pairs phase {phase['name']!r} needs a closed_loop target")
    platforms = workload.get("platforms")
    if platforms is not None:
        if not isinstance(platforms, dict) or not isinstance(platforms.get("required"), list):
            raise ConfigError("platforms.required must be a list when platforms is declared")
        normalized = [normalized_platform_name(str(item)) for item in platforms["required"]]
        if not normalized or len(set(normalized)) != len(normalized):
            raise ConfigError("platforms.required must contain unique normalized platforms")
        unsupported = set(normalized) - {"linux", "windows", "macos"}
        if unsupported:
            raise ConfigError(f"unsupported required platform(s): {sorted(unsupported)}")
        platforms["required"] = normalized
        statuses = platforms.get("status")
        if statuses is not None:
            if not isinstance(statuses, dict):
                raise ConfigError("platforms.status must be an object")
            normalized_statuses = {
                normalized_platform_name(str(key)): value for key, value in statuses.items()
            }
            if set(normalized_statuses) != set(normalized):
                raise ConfigError("platforms.status must cover exactly platforms.required")
            platforms["status"] = normalized_statuses
    return workload


def phase_pending_reason(phase: dict) -> str | None:
    """A phase is pending when any executable step lacks a concrete argv."""
    if phase.get("pending_reason"):
        return str(phase["pending_reason"])
    steps: list[dict] = []
    for key in ("setup", "work", "recover", "teardown"):
        if isinstance(phase.get(key), dict):
            steps.append(phase[key])
    steps.extend(phase.get("steps") or [])
    for evidence in phase.get("evidence") or []:
        if evidence.get("capture") == "stdout_redacted":
            steps.append(evidence)
    for evidence in phase.get("post_crash_evidence") or []:
        if evidence.get("capture") == "stdout_redacted":
            steps.append(evidence)
    if phase.get("kind") == "aa_pairs":
        return None
    for step in steps:
        if step.get("argv") is None:
            return "step has null argv (product command not yet wired)"
    if phase.get("kind") in {"closed_loop", "open_loop", "crash"} and not isinstance(
        phase.get("work"), dict
    ):
        return "missing work command"
    return None


def effective_phase_pending_reason(workload: dict, phase: dict) -> str | None:
    """Include an A/A phase's closed-loop target in pending preflight."""
    reason = phase_pending_reason(phase)
    if reason is not None or phase.get("kind") != "aa_pairs":
        return reason
    target_name = phase.get("target_phase")
    target = next((item for item in workload["phases"] if item["name"] == target_name), None)
    if target is None:
        return f"target phase {target_name!r} is unknown"
    target_reason = phase_pending_reason(target)
    if target_reason is not None:
        return f"target phase {target['name']!r} is pending ({target_reason})"
    return None


def _fingerprint_matches_bound(actual: dict[str, Any], bound: dict[str, Any]) -> bool:
    return (
        actual.get("kind") == bound.get("kind")
        and actual.get("sha256", actual.get("aggregate_sha256")) == bound.get("sha256")
        and actual.get("size_bytes") == bound.get("size_bytes")
        and actual.get("file_count") == bound.get("file_count")
    )


# ---------------------------------------------------------------------------
# Execution context
# ---------------------------------------------------------------------------


class RunContext:
    def __init__(
        self,
        workload: dict,
        input_root: Path,
        output_root: Path,
        base_env: dict,
        forbidden: list[tuple[str, Path]],
        timeout_default: float,
        binary: str | None,
        config_source: Path | None = None,
        bound_corpus: dict[str, Any] | None = None,
        bound_binary: dict[str, Any] | None = None,
        bound_config: dict[str, Any] | None = None,
    ):
        self.workload = workload
        self.input_root = input_root
        self.output_root = output_root
        self.base_env = base_env
        self.forbidden = forbidden
        self.timeout_default = timeout_default
        self.binary = binary
        self.config_source = config_source
        self.bound_corpus = bound_corpus
        self.bound_binary = bound_binary
        self.bound_config = bound_config
        self.phase_evidence: dict[tuple[str, str], dict[str, Any]] = {}
        self.phase_run_dirs: dict[tuple[str, str], Path] = {}
        self.run_state: dict[Path, dict[str, Path]] = {}
        self.runs: list[dict] = []
        self.work_root = create_fresh_directory(output_root / "work", "runner work")

    def _owned_directory(self, path: Path, role: str) -> Path:
        if path.exists():
            safe_expanded_path(str(path), self.output_root, role, require_directory=True)
            return path
        safe_expanded_path(str(path), self.output_root, role)
        return create_fresh_directory(path, role)

    def prepare_run(self, run_dir: Path, phase: dict, family: str) -> None:
        store = copy_safe_tree(
            self.input_root, run_dir / "store", f"phase {phase['name']} family {family}"
        )
        sandbox = create_child_sandbox(run_dir, "child", data_root=store)
        if self.config_source is not None:
            copied_config = copy_safe_artifact(
                self.config_source, sandbox["config"], "frozen config"
            )
            if self.bound_config is not None and not _fingerprint_matches_bound(
                artifact_fingerprint(copied_config, "runner-owned config copy"),
                self.bound_config,
            ):
                raise SafetyError("runner-owned config copy does not match frozen identity")
        if self.bound_corpus is not None:
            copied_fingerprint = {
                "kind": "tree",
                **fingerprint_tree(store, "runner-owned corpus copy"),
            }
            if not _fingerprint_matches_bound(
                copied_fingerprint,
                self.bound_corpus,
            ):
                raise SafetyError("runner-owned corpus copy no longer matches frozen identity")
        self.run_state[run_dir] = {"store": store, **sandbox}

    def state(self, run_dir: Path) -> dict[str, Path]:
        try:
            return self.run_state[run_dir]
        except KeyError as exc:
            raise ExecutionError("run directory was not initialized by the runner") from exc

    def mapping(self, family: str, run_dir: Path, repetition: int = 0) -> dict[str, str]:
        state = self.state(run_dir)
        return {
            "INPUT": str(state["store"]),
            "OUTPUT": str(state["output"]),
            "RUN_DIR": str(run_dir),
            "FAMILY": family,
            "BINARY": self.binary or "",
            "PYTHON": sys.executable,
            "REPETITION": str(repetition),
            "HOME": str(state["home"]),
            "CONFIG": str(state["config"]),
            "CACHE": str(state["cache"]),
            "TRACEDECAY_DATA_DIR": str(state["data"]),
            "TRACEDECAY_GLOBAL_DB": str(state["data"] / "global.db"),
        }

    def path_roots(self, run_dir: Path) -> dict[str, Path]:
        state = self.state(run_dir)
        roots = {
            "INPUT": state["store"],
            "OUTPUT": state["output"],
            "RUN_DIR": run_dir,
            "HOME": state["home"],
            "CONFIG": state["config"],
            "CACHE": state["cache"],
            "TRACEDECAY_DATA_DIR": state["data"],
            "TRACEDECAY_GLOBAL_DB": state["data"],
        }
        if self.binary:
            roots["BINARY"] = Path(self.binary).parent
        return roots

    def child_env(self, run_dir: Path) -> dict[str, str]:
        safety = self.workload.get("safety", {})
        return build_child_env(
            self.base_env,
            dict(safety.get("env") or {}),
            list(safety.get("env_path_keys") or []),
            self.forbidden,
            self.state(run_dir),
        )

    def command(self, step: dict, family: str, run_dir: Path, repetition: int = 0) -> dict:
        if self.binary and self.bound_binary is not None:
            current = binary_identity(self.binary)
            if (
                current["sha256"] != self.bound_binary.get("sha256")
                or current["size_bytes"] != self.bound_binary.get("size_bytes")
            ):
                raise SafetyError("tested binary changed after frozen identity binding")
        argv = substitute_argv(
            step["argv"], self.mapping(family, run_dir, repetition), self.path_roots(run_dir)
        )
        return run_command(
            argv, self.child_env(run_dir), self.timeout(step), cwd=self.state(run_dir)["cwd"]
        )

    def expand_path(self, template: object, family: str, run_dir: Path, role: str) -> Path:
        template_text = str(template)
        mapping = self.mapping(family, run_dir)
        value = substitute(template_text, mapping)
        roots = [
            root
            for token, root in self.path_roots(run_dir).items()
            if f"__{token}__" in template_text
        ]
        if not roots:
            raise ConfigError(f"{role} must use a runner-owned path placeholder")
        # A path cannot safely span independent roots.  Multiple placeholders
        # are permitted only when they still resolve under the same root.
        for root in roots:
            if _path_is_within(value, root):
                return safe_expanded_path(value, root, role)
        raise SafetyError(f"{role} does not remain inside its declared placeholder root")

    def timeout(self, step: dict) -> float:
        defaults = self.workload.get("defaults", {})
        return _config_number(
            step.get("timeout_seconds", defaults.get("timeout_seconds", self.timeout_default)),
            "command timeout_seconds",
            0,
            strict=True,
        )


def fresh_run_dir(ctx: RunContext, phase: dict, family: str, label: str | None) -> Path:
    phase_name = require_safe_identifier(phase["name"], "phase name")
    family = require_safe_identifier(family, "store family")
    parts = [phase_name, family]
    if label:
        parts.append(require_safe_identifier(label, "run label"))
    else:
        parts.append("run")
    phase_dir = ctx._owned_directory(ctx.work_root / parts[0], "phase work directory")
    family_dir = ctx._owned_directory(phase_dir / parts[1], "family work directory")
    run_dir = create_fresh_directory(family_dir / parts[2], "run directory")
    ctx.prepare_run(run_dir, phase, family)
    return run_dir


def relative_to_output(ctx: RunContext, path: Path) -> str:
    safe_expanded_path(str(path), ctx.output_root, "result-relative path")
    return path.relative_to(ctx.output_root).as_posix()


def _sqlite_identifier(value: object, role: str) -> str:
    return require_safe_identifier(value, role)


def capture_logical_sqlite_evidence(target: Path, spec: dict) -> dict[str, Any]:
    """Capture logical SQLite state without publishing raw DB bytes/rows/FTS text."""
    target = assert_safe_path_components(target, "logical SQLite evidence", require_directory=False)
    connection: sqlite3.Connection | None = None
    try:
        connection = sqlite3.connect(f"{target.as_uri()}?mode=ro", uri=True)
        connection.execute("PRAGMA query_only = ON")
        integrity_rows = connection.execute("PRAGMA integrity_check").fetchall()
        schema_rows = connection.execute(
            "SELECT type, name, tbl_name, sql FROM sqlite_master "
            "WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name"
        ).fetchall()
        tables = []
        for raw_name in spec.get("tables") or []:
            name = _sqlite_identifier(raw_name, "SQLite evidence table")
            quoted = '"' + name.replace('"', '""') + '"'
            count = connection.execute(f"SELECT COUNT(*) FROM {quoted}").fetchone()[0]
            tables.append({"table_id": name, "row_count": int(count)})
        fts = []
        fts_probes = spec.get("fts_probes") or []
        if spec.get("require_fts_probes") is True and not fts_probes:
            raise ConfigError("logical SQLite evidence requires at least one FTS probe")
        for probe in fts_probes:
            if not isinstance(probe, dict):
                raise ConfigError("SQLite FTS probes must be objects")
            probe_id = require_safe_identifier(probe.get("name", ""), "SQLite FTS probe")
            table = _sqlite_identifier(probe.get("table", ""), "SQLite FTS table")
            query = probe.get("query")
            if not isinstance(query, str):
                raise ConfigError(f"SQLite FTS probe {probe_id!r} needs a string query")
            limit = _config_int(probe.get("limit", 1000), f"SQLite FTS probe {probe_id!r} limit", 1)
            if limit > 10000:
                raise ConfigError(f"SQLite FTS probe {probe_id!r} limit must be 1..10000")
            projection = probe.get("projection", "rowid")
            if projection not in {"rowid", "rowid_rank_snippet"}:
                raise ConfigError(f"SQLite FTS probe {probe_id!r} has unknown projection")
            quoted = '"' + table.replace('"', '""') + '"'
            if projection == "rowid_rank_snippet":
                rows = connection.execute(
                    f"SELECT rowid, bm25({quoted}), "
                    f"snippet({quoted}, ?, '[', ']', '...', 64) "
                    f"FROM {quoted} WHERE {quoted} MATCH ? "
                    f"ORDER BY bm25({quoted}), rowid LIMIT ?",
                    (-1, query, limit + 1),
                ).fetchall()
            else:
                rows = connection.execute(
                    f"SELECT rowid FROM {quoted} WHERE {quoted} MATCH ? ORDER BY rowid LIMIT ?",
                    (query, limit + 1),
                ).fetchall()
            truncated = len(rows) > limit
            rows = rows[:limit]
            row_ids = [row[0] for row in rows]
            fts.append(
                {
                    "probe_id": probe_id,
                    "projection": projection,
                    "match_count": len(row_ids),
                    "row_identity_sha256": sha256_text(
                        json.dumps(row_ids, separators=(",", ":"), ensure_ascii=False)
                    ),
                    "result_sha256": sha256_text(
                        json.dumps(rows, separators=(",", ":"), ensure_ascii=False)
                    ),
                    "truncated": truncated,
                }
            )
    except (sqlite3.Error, OSError) as exc:
        raise ExecutionError(f"logical SQLite evidence could not be captured: {type(exc).__name__}") from exc
    finally:
        try:
            if connection is not None:
                connection.close()
        except sqlite3.Error:
            pass
    integrity_text = json.dumps(integrity_rows, separators=(",", ":"), ensure_ascii=False)
    schema_text = json.dumps(schema_rows, separators=(",", ":"), ensure_ascii=False)
    return {
        "schema": LOGICAL_SQLITE_EVIDENCE_SCHEMA,
        "integrity": {
            "status": "ok" if integrity_rows == [("ok",)] else "not_ok",
            "result_sha256": sha256_text(integrity_text),
            "result_row_count": len(integrity_rows),
        },
        "schema_sha256": sha256_text(schema_text),
        "tables": tables,
        "fts": fts,
    }


def record_evidence(
    ctx: RunContext,
    phase: dict,
    family: str,
    run_dir: Path,
    evidence_specs: list[dict],
) -> dict:
    captured: dict[str, Any] = {}
    for spec in evidence_specs or []:
        name = spec.get("name")
        if not name:
            raise ConfigError(f"phase {phase['name']!r} evidence entry missing name")
        capture = spec.get("capture")
        if capture == "logical_file":
            target = ctx.expand_path(spec.get("path", ""), family, run_dir, f"evidence {name}")
            target = assert_safe_path_components(target, f"evidence {name}", require_directory=False)
            info = os.lstat(target)
            captured[name] = {
                "schema": "storage-runtime-logical-file-evidence-v1",
                "content_sha256": sha256_file(target, f"evidence {name}"),
                "size_bytes": info.st_size,
            }
        elif capture == "sqlite_logical":
            target = ctx.expand_path(spec.get("path", ""), family, run_dir, f"evidence {name}")
            logical = capture_logical_sqlite_evidence(target, spec)
            if logical["integrity"]["status"] != "ok":
                raise ExecutionError(
                    f"logical SQLite evidence {name!r} failed integrity_check"
                )
            captured[name] = logical
        elif capture == "stdout_redacted":
            result = ctx.command(spec, family, run_dir)
            if not command_succeeded(result, spec.get("expect_exit_code", 0)):
                raise ExecutionError(
                    f"evidence command {name!r} failed in phase {phase['name']!r}: "
                    f"{command_failure_detail(result)}"
                )
            captured[name] = {
                "schema": "storage-runtime-redacted-stdout-evidence-v1",
                "capture": "fts_or_stdout_redacted",
                "output": result["stdout"],
            }
        else:
            raise ConfigError(f"evidence {name!r} has unknown capture {capture!r}")
    ctx.phase_evidence[(phase["name"], family)] = captured
    return captured


def resolve_compare_ref(ctx: RunContext, phase: dict, family: str, ref: str) -> Any:
    if ":" in ref:
        phase_name, name = ref.split(":", 1)
    else:
        phase_name, name = phase["name"], ref
    evidence = ctx.phase_evidence.get((phase_name, family), {})
    if name not in evidence:
        raise ExecutionError(
            f"compare reference {ref!r} has no captured evidence for family {family!r}"
        )
    return evidence[name]


def evaluate_compares(
    ctx: RunContext, phase: dict, family: str, compare_specs: list[dict]
) -> list[dict]:
    results = []
    for spec in compare_specs or []:
        a_value = resolve_compare_ref(ctx, phase, family, spec["a"])
        b_value = resolve_compare_ref(ctx, phase, family, spec["b"])
        expect = spec.get("expect", "equal")
        if expect == "equal":
            passed = a_value == b_value
        elif expect == "different":
            passed = a_value != b_value
        else:
            raise ConfigError(f"unknown compare expectation {expect!r}")
        results.append(
            {
                "a": spec["a"],
                "b": spec["b"],
                "expect": expect,
                "pass": passed,
            }
        )
    return results


# ---------------------------------------------------------------------------
# Phase executors
# ---------------------------------------------------------------------------


def execute_setup(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> None:
    setup = phase.get("setup")
    if not isinstance(setup, dict):
        return
    result = ctx.command(setup, family, run_dir)
    if not command_succeeded(result, setup.get("expect_exit_code", 0)):
        raise ExecutionError(
            f"setup failed for phase {phase['name']!r} family {family!r}: "
            f"{command_failure_detail(result)}"
        )


def execute_closed_loop(
    ctx: RunContext, phase: dict, family: str, run_dir: Path
) -> dict:
    work = phase["work"]
    defaults = ctx.workload.get("defaults", {})
    warmup = _config_int(phase.get("warmup", defaults.get("warmup", 0)), "warmup", 0)
    repetitions = _config_int(
        phase.get("repetitions", defaults.get("repetitions", 1)), "repetitions", 1
    )
    outcome_map = {**DEFAULT_OUTCOME_MAP, **(phase.get("outcome_map") or {})}

    counts = new_counts()
    samples: list[dict] = []
    latencies: list[int] = []

    for index in range(warmup + repetitions):
        measured = index >= warmup
        issue = time.monotonic_ns()
        result = ctx.command(work, family, run_dir, index)
        finished = time.monotonic_ns()
        if not measured:
            continue
        outcome = map_outcome(result["exit_code"], result["timed_out"], outcome_map)
        if result["process_tree"].get("clean") == "false":
            outcome = "failed"
        counts["offered"] += 1
        counts["admitted"] += 1
        if outcome == "completed":
            counts["completed"] += 1
        else:
            counts["failed"] += 1
        latency = finished - issue
        latencies.append(latency)
        samples.append(
            {
                "operation": index - warmup,
                "latency_ns": latency,
                "exit_code": result["exit_code"],
                "timed_out": result["timed_out"],
                "outcome": outcome,
                "process_tree": result["process_tree"],
            }
        )

    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    wall_ns = sum(latencies)
    return {
        "counts": counts,
        "latency": {
            # Closed loop: each operation is issued only after the previous
            # one completed, so issue time equals scheduled time and the
            # distribution carries no coordinated omission.
            "response_ns": summarize_latency(latencies),
        },
        "throughput_ops_per_second": (
            counts["completed"] / (wall_ns / 1e9) if wall_ns > 0 else None
        ),
        "samples": samples,
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_open_loop(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> dict:
    work = phase["work"]
    rate = _config_number(
        phase["offered_rate_per_second"], "open_loop offered_rate_per_second", 0, strict=True
    )
    operation_count = _config_int(phase["operation_count"], "open_loop operation_count", 1)
    max_in_flight = _config_int(phase["max_in_flight"], "open_loop max_in_flight", 1)
    outcome_map = {**DEFAULT_OUTCOME_MAP, **(phase.get("outcome_map") or {})}
    retryable = set(phase.get("retryable_outcomes") or ["shed"])
    max_retries = _config_int(phase.get("max_retries", 0), "open_loop max_retries", 0)

    counts = new_counts()
    counts_lock = threading.Lock()
    in_flight = {"value": 0}
    requests: list[dict[str, Any] | None] = [None] * operation_count
    requests_lock = threading.Lock()
    latencies: list[int] = []
    schedule_lags: list[int] = []
    start_ns = time.monotonic_ns()

    def offset(timestamp_ns: int) -> int:
        return timestamp_ns - start_ns

    def worker(op_index: int, scheduled_ns: int, request: dict[str, Any]) -> None:
        attempts = 0
        final_outcome = "failed"
        final_exit = None
        final_timed_out = False
        finished_ns = scheduled_ns
        request["started_at_ns"] = offset(time.monotonic_ns())
        try:
            while True:
                result = ctx.command(work, family, run_dir, op_index)
                finished_ns = time.monotonic_ns()
                outcome = map_outcome(result["exit_code"], result["timed_out"], outcome_map)
                if result["process_tree"].get("clean") == "false":
                    outcome = "failed"
                final_outcome = outcome
                final_exit = result["exit_code"]
                final_timed_out = result["timed_out"]
                if outcome in retryable and attempts < max_retries:
                    attempts += 1
                    continue
                break
        except RunnerError as exc:
            finished_ns = time.monotonic_ns()
            final_outcome = "failed"
            request["error_class"] = type(exc).__name__
        with counts_lock:
            counts["retried"] += attempts
            if final_outcome == "completed":
                counts["completed"] += 1
            elif final_outcome == "shed":
                counts["shed"]["command_saturation"] += 1
            else:
                counts["failed"] += 1
            in_flight["value"] -= 1
        latency = finished_ns - scheduled_ns
        with requests_lock:
            latencies.append(latency)
            request.update(
                {
                    # Latency is measured from the scheduled issue time, not
                    # service start, so queueing delay is retained.
                    "terminal_at_ns": offset(finished_ns),
                    "latency_ns": latency,
                    "attempts": attempts + 1,
                    "exit_code": final_exit,
                    "timed_out": final_timed_out,
                    "outcome": (
                        "shed_command_saturation"
                        if final_outcome == "shed"
                        else final_outcome
                    ),
                    "terminal": True,
                }
            )

    threads: list[threading.Thread] = []
    for op_index in range(operation_count):
        scheduled_ns = start_ns + int(op_index * 1e9 / rate)
        now = time.monotonic_ns()
        if scheduled_ns > now:
            time.sleep((scheduled_ns - now) / 1e9)
        issue_ns = time.monotonic_ns()
        request: dict[str, Any] = {
            "request_id": op_index,
            "scheduled_at_ns": offset(scheduled_ns),
            "admitted_at_ns": None,
            "started_at_ns": None,
            "terminal_at_ns": None,
            "terminal": False,
        }
        with requests_lock:
            requests[op_index] = request
            schedule_lags.append(issue_ns - scheduled_ns)
        with counts_lock:
            counts["offered"] += 1
            if in_flight["value"] >= max_in_flight:
                counts["shed"]["runner_in_flight_cap"] += 1
                request.update(
                    {
                        "terminal_at_ns": offset(issue_ns),
                        "latency_ns": issue_ns - scheduled_ns,
                        "attempts": 0,
                        "exit_code": None,
                        "timed_out": False,
                        "outcome": "shed_runner_in_flight_cap",
                        "terminal": True,
                    }
                )
                with requests_lock:
                    latencies.append(issue_ns - scheduled_ns)
                continue
            in_flight["value"] += 1
            counts["admitted"] += 1
            request["admitted_at_ns"] = offset(issue_ns)
        thread = threading.Thread(
            target=worker, args=(op_index, scheduled_ns, request), daemon=True
        )
        thread.start()
        threads.append(thread)
    for thread in threads:
        thread.join()
    workload_finished_ns = time.monotonic_ns()

    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    terminal_requests = [request for request in requests if request is not None]
    if len(terminal_requests) != operation_count or any(
        not request.get("terminal") or request.get("terminal_at_ns") is None
        for request in terminal_requests
    ):
        raise ExecutionError("open-loop request ledger is missing a terminal record")
    return {
        "counts": counts,
        "latency": {
            "response_ns": summarize_latency(latencies),
            "schedule_lag_ns": summarize_latency(schedule_lags),
        },
        "throughput_ops_per_second": (
            counts["completed"]
            / ((workload_finished_ns - start_ns) / 1e9)
            if operation_count
            else None
        ),
        "requests": terminal_requests,
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_crash(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> dict:
    capability = process_tree_capability()
    if capability["state"] != "supported_best_effort":
        raise ExecutionError(
            "crash phase is unsupported without safe stdlib process-tree control "
            f"({capability['state']})"
        )
    work = phase["work"]
    wait_for_file = phase.get("wait_for_file")
    wait_timeout = _config_number(
        phase.get("wait_timeout_seconds", 30.0), "crash wait_timeout_seconds", 0, strict=True
    )
    after_seconds = _config_number(
        phase.get("after_seconds", 1.0), "crash after_seconds", 0, strict=True
    )

    started = time.monotonic_ns()
    argv = substitute_argv(
        work["argv"], ctx.mapping(family, run_dir), ctx.path_roots(run_dir)
    )
    try:
        proc = subprocess.Popen(
            argv,
            env=ctx.child_env(run_dir),
            cwd=str(ctx.state(run_dir)["cwd"]),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            **_popen_group_kwargs(),
        )
    except OSError as exc:
        raise ExecutionError(
            f"failed to execute crash command {Path(argv[0]).name!r}: {type(exc).__name__}"
        ) from exc
    tree_result: dict[str, str] | None = None
    try:
        if wait_for_file:
            target = ctx.expand_path(wait_for_file, family, run_dir, "crash wait_for_file")
            deadline = started + int(wait_timeout * 1e9)
            while True:
                assert_safe_path_components(target, "crash wait_for_file", allow_missing=True)
                if target.exists():
                    break
                if time.monotonic_ns() > deadline:
                    tree_result = terminate_process_tree(proc)
                    raise ExecutionError(
                        f"crash phase {phase['name']!r} family {family!r}: "
                        f"wait trigger did not appear within {wait_timeout}s"
                    )
                if proc.poll() is not None:
                    raise ExecutionError(
                        f"crash phase {phase['name']!r} family {family!r}: work "
                        f"process exited {proc.returncode} before the kill trigger"
                    )
                time.sleep(0.01)
        else:
            time.sleep(after_seconds)
        killed_at = time.monotonic_ns()
        tree_result = kill_process_tree(proc)
        if tree_result["clean"] != "true":
            raise ExecutionError("crash process group termination could not be verified")
    finally:
        if proc.poll() is None or _posix_group_has_live_members(proc.pid):
            tree_result = terminate_process_tree(proc)

    evidence = record_evidence(
        ctx, phase, family, run_dir, phase.get("post_crash_evidence") or []
    )
    ctx.phase_run_dirs[(phase["name"], family)] = run_dir
    return {
        "counts": new_counts(),
        "crash": {
            "mechanism": "sigkill" if os.name == "posix" else "terminate_process",
            "uptime_before_kill_ns": killed_at - started,
            "work_exit_code": proc.returncode,
            "process_tree": tree_result,
        },
        "evidence": evidence,
        "comparisons": [],
    }


def execute_recovery(ctx: RunContext, phase: dict, family: str, run_dir: Path) -> dict:
    recover = phase.get("recover")
    counts = new_counts()
    if isinstance(recover, dict):
        result = ctx.command(recover, family, run_dir)
        counts["offered"] += 1
        counts["admitted"] += 1
        ok = command_succeeded(result, recover.get("expect_exit_code", 0))
        counts["completed" if ok else "failed"] += 1
        if not ok:
            raise ExecutionError(
                f"recovery command failed for phase {phase['name']!r} family "
                f"{family!r}: {command_failure_detail(result)}"
            )
    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    failures = [item for item in compares if not item["pass"]]
    if failures:
        raise ExecutionError(
            f"recovery phase {phase['name']!r} family {family!r} compare "
            f"failures: {failures}"
        )
    return {
        "counts": counts,
        "recovered_against": relative_to_output(ctx, run_dir),
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_backup_restore(
    ctx: RunContext, phase: dict, family: str, run_dir: Path
) -> dict:
    steps = phase.get("steps") or []
    if not steps:
        raise ConfigError(f"backup_restore phase {phase['name']!r} has no steps")
    step_results = []
    for step in steps:
        require_safe_identifier(step.get("name", "step"), "backup_restore step name")
        result = ctx.command(step, family, run_dir)
        ok = command_succeeded(result, step.get("expect_exit_code", 0))
        step_results.append(
            {
                "name": step.get("name"),
                "exit_code": result["exit_code"],
                "timed_out": result["timed_out"],
                "wall_ns": result["wall_ns"],
                "pass": ok,
                "process_tree": result["process_tree"],
            }
        )
        if not ok:
            raise ExecutionError(
                f"backup_restore step {step.get('name')!r} failed in phase "
                f"{phase['name']!r} family {family!r}: {command_failure_detail(result)}"
            )
    evidence = record_evidence(ctx, phase, family, run_dir, phase.get("evidence") or [])
    compares = evaluate_compares(ctx, phase, family, phase.get("compare") or [])
    failures = [item for item in compares if not item["pass"]]
    if failures:
        raise ExecutionError(
            f"backup_restore phase {phase['name']!r} family {family!r} compare "
            f"failures: {failures}"
        )
    return {
        "counts": new_counts(),
        "steps": step_results,
        "evidence": evidence,
        "comparisons": compares,
    }


def execute_aa_pairs(ctx: RunContext, phase: dict, family: str) -> dict:
    target_name = phase["target_phase"]
    target = next(
        (item for item in ctx.workload["phases"] if item["name"] == target_name),
        None,
    )
    if target is None:
        raise ConfigError(
            f"aa_pairs phase {phase['name']!r} targets unknown phase {target_name!r}"
        )
    if target.get("kind") != "closed_loop":
        raise ConfigError(
            f"aa_pairs target {target_name!r} must be a closed_loop phase"
        )
    pairs = _config_int(phase.get("pairs", 5), "aa_pairs pairs", 1)
    margin_multiplier = _config_number(
        phase.get("margin_multiplier", 2.0), "aa_pairs margin_multiplier", 0, strict=True
    )

    observations: list[dict] = []
    for pair_index in range(pairs):
        for member in ("A", "B"):
            label = f"pair{pair_index}_{member}"
            run_dir = fresh_run_dir(ctx, phase, family, label)
            execute_setup(ctx, target, family, run_dir)
            body = execute_closed_loop(ctx, target, family, run_dir)
            latency = body["latency"]["response_ns"]
            throughput = body["throughput_ops_per_second"]
            observations.append(
                {
                    "pair": pair_index,
                    "member": member,
                    "run_dir": relative_to_output(ctx, run_dir),
                    "p50_response_ns": latency["p50_ns"],
                    "throughput_ops_per_second": throughput,
                    "completed": body["counts"]["completed"],
                }
            )
            ctx.runs.append(
                {
                    "phase": phase["name"],
                    "family": family,
                    "kind": "closed_loop",
                    "repetition_label": label,
                    "status": "completed",
                    **body,
                }
            )

    deltas: list[dict] = []
    for pair_index in range(pairs):
        member_a = observations[2 * pair_index]
        member_b = observations[2 * pair_index + 1]
        pair_delta: dict[str, object] = {"pair": pair_index}
        for metric in ("p50_response_ns", "throughput_ops_per_second"):
            value_a = member_a[metric]
            value_b = member_b[metric]
            if value_a is None or value_b is None:
                pair_delta[f"{metric}_relative_delta"] = None
                continue
            midpoint = (value_a + value_b) / 2.0
            pair_delta[f"{metric}_relative_delta"] = (
                abs(value_a - value_b) / midpoint if midpoint > 0 else 0.0
            )
        deltas.append(pair_delta)

    noise_floor = {}
    for metric in ("p50_response_ns", "throughput_ops_per_second"):
        values = [
            item[f"{metric}_relative_delta"]
            for item in deltas
            if item[f"{metric}_relative_delta"] is not None
        ]
        floor = max(values) if values else None
        noise_floor[metric] = {
            "aa_noise_floor_relative": floor,
            "regression_margin_relative": (
                floor * margin_multiplier if floor is not None else None
            ),
        }

    return {
        "counts": new_counts(),
        "aa": {
            "target_phase": target_name,
            "pairs": pairs,
            "margin_multiplier": margin_multiplier,
            "observations": observations,
            "pair_relative_deltas": deltas,
            "noise_floor": noise_floor,
            "note": (
                "A/A margins are per-machine noise floors; regression gates must "
                "be re-baselined per platform (Linux/Windows/macOS)."
            ),
        },
    }


def execute_phase_for_family(
    ctx: RunContext, phase: dict, family: str, allow_pending: bool
) -> None:
    reason = effective_phase_pending_reason(ctx.workload, phase)
    if reason is not None:
        if not allow_pending:
            raise ConfigError(
                f"phase {phase['name']!r} is pending ({reason}); refusing to "
                f"execute. Re-run with --allow-pending to record it as not run."
            )
        ctx.runs.append(
            {
                "phase": phase["name"],
                "family": family,
                "kind": phase["kind"],
                "status": "pending",
                "pending_reason": reason,
            }
        )
        return

    if phase["kind"] == "aa_pairs":
        body = execute_aa_pairs(ctx, phase, family)
        ctx.runs.append(
            {
                "phase": phase["name"],
                "family": family,
                "kind": phase["kind"],
                "status": "completed",
                **body,
            }
        )
        return

    if phase["kind"] == "recovery":
        source_dir = ctx.phase_run_dirs.get((phase["depends_on"], family))
        if source_dir is None:
            raise ExecutionError(
                f"recovery phase {phase['name']!r} has no crashed runner-owned store copy"
            )
        run_dir = source_dir
        body = execute_recovery(ctx, phase, family, run_dir)
    else:
        run_dir = fresh_run_dir(ctx, phase, family, None)
        execute_setup(ctx, phase, family, run_dir)
        if phase["kind"] == "closed_loop":
            body = execute_closed_loop(ctx, phase, family, run_dir)
        elif phase["kind"] == "open_loop":
            body = execute_open_loop(ctx, phase, family, run_dir)
        elif phase["kind"] == "crash":
            body = execute_crash(ctx, phase, family, run_dir)
        elif phase["kind"] == "backup_restore":
            body = execute_backup_restore(ctx, phase, family, run_dir)
        else:  # pragma: no cover - guarded by load_workload
            raise ConfigError(f"unknown phase kind {phase['kind']!r}")
    ctx.runs.append(
        {
            "phase": phase["name"],
            "family": family,
            "kind": phase["kind"],
            "run_dir": relative_to_output(ctx, run_dir),
            "status": "completed",
            **body,
        }
    )


# ---------------------------------------------------------------------------
# Result validation
# ---------------------------------------------------------------------------

RESULT_REQUIRED_KEYS = {
    "artifact_id",
    "schema_version",
    "status",
    "evidence_status",
    "execution_scope",
    "workload",
    "frozen_identity",
    "identity_binding",
    "environment",
    "platform",
    "process_tree_control",
    "safety",
    "logical_evidence_schema",
    "input_fingerprint",
    "runs",
    "limitations",
}


def validate_open_loop_ledger(run: dict, counts: dict) -> list[str]:
    phase = run.get("phase")
    requests = run.get("requests")
    if not isinstance(requests, list):
        return [f"run {phase!r} missing overload request ledger"]
    problems: list[str] = []
    if len(requests) != counts["offered"]:
        problems.append(f"run {phase!r}: request ledger count != offered")
    ids = [request.get("request_id") for request in requests if isinstance(request, dict)]
    valid_ids = all(isinstance(value, int) and not isinstance(value, bool) for value in ids)
    if not valid_ids or len(ids) != len(requests) or len(set(ids)) != len(ids):
        problems.append(f"run {phase!r}: request ids are not unique")
    outcomes = {
        "completed": 0,
        "failed": 0,
        "shed_command_saturation": 0,
        "shed_runner_in_flight_cap": 0,
    }
    retries = 0
    for request in requests:
        if not isinstance(request, dict) or not request.get("terminal"):
            problems.append(f"run {phase!r}: offered request lacks terminal record")
            continue
        for key in ("scheduled_at_ns", "admitted_at_ns", "started_at_ns", "terminal_at_ns"):
            if key not in request:
                problems.append(f"run {phase!r}: request missing {key}")
        outcome = request.get("outcome")
        if request.get("terminal_at_ns") is None or outcome not in outcomes:
            problems.append(f"run {phase!r}: request terminal outcome is incomplete")
            continue
        outcomes[outcome] += 1
        attempts = request.get("attempts")
        if not isinstance(attempts, int) or isinstance(attempts, bool) or attempts < 0:
            problems.append(f"run {phase!r}: request attempts is invalid")
            continue
        if outcome == "shed_runner_in_flight_cap":
            if attempts != 0 or request.get("admitted_at_ns") is not None or request.get(
                "started_at_ns"
            ) is not None:
                problems.append(f"run {phase!r}: runner-shed request was marked admitted")
        else:
            if attempts < 1 or request.get("admitted_at_ns") is None or request.get(
                "started_at_ns"
            ) is None:
                problems.append(f"run {phase!r}: admitted request timing is incomplete")
            retries += max(0, attempts - 1)
        ordered_times: list[int] = []
        invalid_time = False
        for key in ("scheduled_at_ns", "admitted_at_ns", "started_at_ns", "terminal_at_ns"):
            value = request.get(key)
            if value is None:
                continue
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                invalid_time = True
                continue
            ordered_times.append(value)
        if invalid_time or any(
            earlier > later for earlier, later in zip(ordered_times, ordered_times[1:])
        ):
            problems.append(f"run {phase!r}: request timing is invalid")
    expected = {
        "completed": counts["completed"],
        "failed": counts["failed"],
        "shed_command_saturation": counts["shed"]["command_saturation"],
        "shed_runner_in_flight_cap": counts["shed"]["runner_in_flight_cap"],
    }
    if outcomes != expected:
        problems.append(f"run {phase!r}: request outcomes do not match aggregate counts")
    if retries != counts["retried"]:
        problems.append(f"run {phase!r}: request attempts do not match retried count")
    return problems


def identity_components_valid(components: object) -> bool:
    required = {"binary", "schema_manifest", "workload", "corpus", "config"}
    if not isinstance(components, dict) or set(components) != required:
        return False
    return all(
        isinstance(component, dict)
        and component.get("verified") is True
        and component.get("kind") in {"file", "tree"}
        and isinstance(component.get("sha256"), str)
        and SHA256_HEX.fullmatch(component["sha256"]) is not None
        for component in components.values()
    )


def redacted_summary_valid(summary: object) -> bool:
    return (
        isinstance(summary, dict)
        and summary.get("redacted") is True
        and isinstance(summary.get("sha256"), str)
        and SHA256_HEX.fullmatch(summary["sha256"]) is not None
        and all(
            isinstance(summary.get(key), int)
            and not isinstance(summary.get(key), bool)
            and summary[key] >= 0
            for key in ("byte_count", "line_count")
        )
    )


def validate_result(result: dict) -> list[str]:
    problems: list[str] = []
    missing = RESULT_REQUIRED_KEYS - set(result)
    if missing:
        problems.append(f"missing result keys: {sorted(missing)}")
        return problems
    if result["artifact_id"] != RESULT_ARTIFACT_ID:
        problems.append(f"artifact_id must be {RESULT_ARTIFACT_ID!r}")
    if result["schema_version"] != RESULT_SCHEMA_VERSION:
        problems.append(f"schema_version must be {RESULT_SCHEMA_VERSION}")
    if result.get("logical_evidence_schema") != LOGICAL_SQLITE_EVIDENCE_SCHEMA:
        problems.append("logical_evidence_schema is missing or unsupported")
    if result.get("status") not in {"completed", "not_evidence", "failed_validation"}:
        problems.append("result status must be completed, not_evidence, or failed_validation")
    raw_evidence_status = result.get("evidence_status")
    evidence_status = raw_evidence_status if isinstance(raw_evidence_status, dict) else {}
    if evidence_status.get("state") not in {
        "evidence",
        "not_evidence",
    }:
        problems.append("evidence_status must explicitly state evidence or not_evidence")
    raw_scope = result.get("execution_scope")
    scope = raw_scope if isinstance(raw_scope, dict) else {}
    if scope.get("mode") not in {"full", "partial"}:
        problems.append("execution_scope must explicitly state full or partial")
    raw_environment = result.get("environment")
    env_block = raw_environment if isinstance(raw_environment, dict) else {}
    if not isinstance(raw_environment, dict):
        problems.append("environment must be an object")
    for key in (
        "os",
        "platform_id",
        "machine",
        "python_version",
        "captured_at_utc",
        "runner_version",
    ):
        if key not in env_block:
            problems.append(f"environment missing {key!r}")
    platform_block = result.get("platform", {})
    if not isinstance(platform_block, dict) or not platform_block.get("current"):
        problems.append("platform block missing normalized current platform")
    raw_process_tree = result.get("process_tree_control", {})
    process_tree = raw_process_tree if isinstance(raw_process_tree, dict) else {}
    if not process_tree.get("state"):
        problems.append("process_tree_control is missing")
    raw_workload = result.get("workload")
    workload_block = raw_workload if isinstance(raw_workload, dict) else {}
    if not isinstance(raw_workload, dict):
        problems.append("workload must be an object")

    incomplete_runs = False
    raw_runs = result.get("runs")
    runs = raw_runs if isinstance(raw_runs, list) else []
    if not isinstance(raw_runs, list):
        problems.append("runs must be a list")
        incomplete_runs = True
    for run in runs:
        if not isinstance(run, dict):
            problems.append("run entries must be objects")
            incomplete_runs = True
            continue
        if run.get("status") not in {"completed", "pending", "failed", "partial", "not_run"}:
            problems.append(f"run {run.get('phase')!r} has an invalid status")
            incomplete_runs = True
            continue
        if run.get("status") in {"pending", "failed", "partial", "not_run"}:
            incomplete_runs = True
            continue
        counts = run.get("counts")
        if counts is None:
            problems.append(f"run {run.get('phase')!r} missing counts")
            continue
        for violation in counts_invariants_ok(counts):
            problems.append(f"run {run.get('phase')!r}: {violation}")
        if isinstance(counts, dict) and isinstance(counts.get("failed"), int) and counts["failed"]:
            problems.append(f"run {run.get('phase')!r}: failed operations are not evidence")
        for comparison in run.get("comparisons", []) or []:
            if not comparison.get("pass"):
                problems.append(
                    f"run {run.get('phase')!r}: failed comparison {comparison}"
                )
        if run.get("kind") == "open_loop":
            if not counts_invariants_ok(counts):
                problems.extend(validate_open_loop_ledger(run, counts))
        raw_evidence = run.get("evidence") or {}
        if not isinstance(raw_evidence, dict):
            problems.append(f"run {run.get('phase')!r}: evidence must be an object")
            raw_evidence = {}
        for value in raw_evidence.values():
            if not isinstance(value, dict) or "schema" not in value:
                problems.append(
                    f"run {run.get('phase')!r}: evidence must use a logical/redacted schema"
                )
            elif value["schema"] == "storage-runtime-redacted-stdout-evidence-v1":
                if not redacted_summary_valid(value.get("output")):
                    problems.append(f"run {run.get('phase')!r}: stdout evidence is not redacted")
            elif value["schema"] == LOGICAL_SQLITE_EVIDENCE_SCHEMA:
                integrity = value.get("integrity")
                if not isinstance(integrity, dict) or integrity.get("status") != "ok":
                    problems.append(f"run {run.get('phase')!r}: SQLite integrity is not ok")
            elif value["schema"] == "storage-runtime-logical-file-evidence-v1":
                if workload_block.get("evidence_eligible") is True:
                    problems.append(
                        f"run {run.get('phase')!r}: product evidence uses a synthetic logical file"
                    )
            else:
                problems.append(f"run {run.get('phase')!r}: unknown evidence schema")

    if result.get("status") == "completed":
        if evidence_status.get("state") != "evidence":
            problems.append("completed result may not be marked not_evidence")
        if scope.get("mode") != "full":
            problems.append("partial/--only result must never be completed")
        if incomplete_runs:
            problems.append("pending or failed run makes completed result invalid")
        raw_binding = result.get("identity_binding")
        binding = raw_binding if isinstance(raw_binding, dict) else {}
        if binding.get("status") != "bound":
            problems.append("completed evidence must be bound to frozen identity")
        elif not identity_components_valid(binding.get("components")):
            problems.append("completed evidence has malformed identity components")
        raw_frozen = result.get("frozen_identity")
        frozen = raw_frozen if isinstance(raw_frozen, dict) else {}
        if (
            frozen.get("status") != "supplied"
            or not isinstance(frozen.get("sha256"), str)
            or SHA256_HEX.fullmatch(frozen["sha256"]) is None
        ):
            problems.append("completed evidence requires a supplied frozen identity")
        if process_tree.get("state") != "supported_best_effort":
            problems.append("completed evidence requires verified process-tree capability")
        if workload_block.get("evidence_eligible") is not True:
            problems.append("completed evidence requires an evidence-eligible workload")
        if not isinstance(workload_block.get("sha256"), str) or SHA256_HEX.fullmatch(
            workload_block["sha256"]
        ) is None:
            problems.append("completed evidence requires a workload hash")
        input_fingerprint = result.get("input_fingerprint")
        if (
            not isinstance(input_fingerprint, dict)
            or not isinstance(input_fingerprint.get("aggregate_sha256"), str)
            or SHA256_HEX.fullmatch(input_fingerprint["aggregate_sha256"]) is None
        ):
            problems.append("completed evidence requires an input fingerprint")
        raw_safety = result.get("safety")
        safety = raw_safety if isinstance(raw_safety, dict) else {}
        input_fs = safety.get("input_filesystem")
        output_fs = safety.get("output_filesystem")
        if (
            not isinstance(input_fs, dict)
            or not isinstance(output_fs, dict)
            or input_fs.get("state") != "local"
            or output_fs.get("state") != "local"
        ):
            problems.append("completed evidence requires verified local filesystems")
        if not runs:
            problems.append("completed evidence requires at least one run")
    elif evidence_status.get("state") == "evidence":
        problems.append("non-completed result may not claim evidence")
    return problems


def result_contains_absolute_paths(result: dict) -> list[str]:
    """Machine-specific absolute paths must never enter a tracked artifact."""
    hits: list[str] = []

    def scan(node, trail: str) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                scan(value, f"{trail}.{key}")
        elif isinstance(node, list):
            for index, value in enumerate(node):
                scan(value, f"{trail}[{index}]")
        elif isinstance(node, str):
            if node.startswith(("/", "\\")) or (
                len(node) > 2 and node[1] == ":" and node[2] in "\\/"
            ):
                hits.append(trail)

    scan(result, "$")
    return hits


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------


def load_safe_json(path_like: str | Path, role: str) -> tuple[Path, dict]:
    path = assert_safe_path_components(path_like, role, require_directory=False)
    try:
        with os.fdopen(_open_read_no_follow(path, role), "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise ConfigError(f"cannot load {role}: {type(exc).__name__}") from exc
    if not isinstance(value, dict):
        raise ConfigError(f"{role} must contain a JSON object")
    return path, value


def file_fingerprint(path_like: str | Path, role: str) -> dict[str, Any]:
    fingerprint = artifact_fingerprint(path_like, role)
    if fingerprint["kind"] != "file":
        raise ConfigError(f"{role} must be a regular file")
    return fingerprint


def freeze_version_probe(
    binary_path: Path,
    version_args: list[str],
    forbidden: list[tuple[str, Path]],
) -> dict[str, Any]:
    """Best-effort binary version metadata under the same isolated child policy."""
    if not version_args:
        return {"status": "not_requested"}
    if process_tree_capability()["state"] != "supported_best_effort":
        return {"status": "not_run_process_tree_unsupported"}
    with tempfile.TemporaryDirectory(prefix="tracedecay-s0-version-") as temporary:
        probe_root = create_fresh_directory(Path(temporary) / "probe", "freeze version probe")
        sandbox = create_child_sandbox(probe_root, "freeze version probe")
        env = build_child_env(
            safe_probe_base_env(dict(os.environ)), {}, [], forbidden, sandbox
        )
        try:
            result = run_command(
                [str(binary_path), *[str(arg) for arg in version_args]],
                env,
                30.0,
                cwd=sandbox["cwd"],
            )
            probe = {
                "status": "available" if command_succeeded(result) else "unavailable",
                "exit_code": result["exit_code"],
                "output": preferred_output_summary(result),
                "process_tree": result["process_tree"],
            }
        except RunnerError:
            probe = {"status": "unavailable"}
        validate_safe_tree(probe_root, "freeze version probe output")
        return probe


def _identity_component_match(expected: dict | None, actual: dict) -> bool:
    """Compare only immutable fingerprint fields; no path is an identity input."""
    if not isinstance(expected, dict):
        return False
    if expected.get("kind") != actual.get("kind"):
        return False
    if expected.get("kind") == "file":
        return (
            expected.get("sha256") == actual.get("sha256")
            and expected.get("size_bytes") == actual.get("size_bytes")
        )
    if expected.get("kind") == "tree":
        return (
            expected.get("aggregate_sha256") == actual.get("aggregate_sha256")
            and expected.get("file_count") == actual.get("file_count")
        )
    return False


def bind_frozen_identity(
    identity: dict,
    *,
    binary_path: str | Path,
    schema_manifest_path: str | Path,
    workload_path: Path,
    corpus_root: Path,
    config_path: str | Path,
) -> dict[str, Any]:
    """Fail closed unless every artifact tested by this run matches the freeze."""
    if identity.get("artifact_id") != IDENTITY_ARTIFACT_ID:
        raise ConfigError("frozen identity has an unsupported artifact_id")
    if identity.get("schema_version") != IDENTITY_SCHEMA_VERSION:
        raise ConfigError("frozen identity has an unsupported schema_version")
    expected = {
        "binary": identity.get("binary"),
        "schema_manifest": identity.get("schema_manifest"),
        "workload": identity.get("workload"),
        "corpus": identity.get("corpus"),
        "config": identity.get("config"),
    }
    if any(not isinstance(value, dict) for value in expected.values()):
        raise ConfigError("frozen identity is missing one or more bound artifact fingerprints")
    binary = binary_identity(binary_path)
    actual = {
        "binary": {"kind": "file", **binary},
        "schema_manifest": file_fingerprint(schema_manifest_path, "schema manifest"),
        "workload": file_fingerprint(workload_path, "workload"),
        "corpus": artifact_fingerprint(corpus_root, "corpus"),
        "config": artifact_fingerprint(config_path, "config"),
    }
    mismatches = [
        key
        for key, expected_value in expected.items()
        if not _identity_component_match(expected_value, actual[key])
    ]
    if mismatches:
        raise ConfigError(
            "frozen identity does not match tested artifacts: " + ", ".join(sorted(mismatches))
        )
    return {
        "status": "bound",
        "components": {
            key: {
                "kind": value["kind"],
                "sha256": value.get("sha256", value.get("aggregate_sha256")),
                "size_bytes": value.get("size_bytes"),
                "file_count": value.get("file_count"),
                "verified": True,
            }
            for key, value in actual.items()
        },
    }


def cmd_freeze(args: argparse.Namespace) -> int:
    home = Path.home()
    forbidden = forbidden_profile_roots(dict(os.environ), home)
    output_path = guard_path(args.output, "frozen identity output", forbidden)
    _safe_mkdir_parents(output_path.parent, "frozen identity output")
    assert_safe_path_components(
        output_path.parent, "frozen identity output", require_directory=True
    )

    binary_path = guard_path(args.binary, "binary", forbidden)
    schema_manifest_path = guard_path(args.schema_manifest, "schema manifest", forbidden)
    workload_path = guard_path(args.workload, "workload", forbidden)
    corpus_path = guard_path(args.corpus, "corpus", forbidden)
    config_path = guard_path(args.config, "config", forbidden)
    validate_safe_tree(corpus_path, "corpus")
    for path, role in (
        (binary_path, "binary"),
        (schema_manifest_path, "schema manifest"),
        (workload_path, "workload"),
        (corpus_path, "corpus"),
        (config_path, "config"),
        (output_path.parent, "frozen identity output"),
    ):
        reject_network_filesystem(path, role)
    binary = {"kind": "file", **binary_identity(binary_path)}
    binary["version_probe"] = freeze_version_probe(
        binary_path, list(args.binary_version_argv or []), forbidden
    )
    families = args.store_family or ["graph", "profile", "project", "session"]
    for family in families:
        require_safe_identifier(family, "store family")
    if len(set(families)) != len(families):
        raise ConfigError("store families must be unique")
    identity = {
        "artifact_id": IDENTITY_ARTIFACT_ID,
        "schema_version": IDENTITY_SCHEMA_VERSION,
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "captured_by": f"run_storage_baseline.py {RUNNER_VERSION}",
        "binary": binary,
        "schema_manifest": file_fingerprint(schema_manifest_path, "schema manifest"),
        "workload": file_fingerprint(workload_path, "workload"),
        "corpus": artifact_fingerprint(corpus_path, "corpus"),
        "config": artifact_fingerprint(config_path, "config"),
        "store_families": families,
        "notes": args.notes or "",
    }
    identity_path_leaks = result_contains_absolute_paths(identity)
    if identity_path_leaks:
        raise ConfigError(
            "frozen identity notes/metadata may not contain absolute paths: "
            + ", ".join(identity_path_leaks)
        )
    atomic_write_new(
        output_path, json.dumps(identity, indent=2, sort_keys=True) + "\n", "frozen identity"
    )
    print(f"[s0] frozen identity written to {output_path}", file=sys.stderr)
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    home = Path.home()
    forbidden = forbidden_profile_roots(dict(os.environ), home)
    workload_path = guard_path(args.workload, "workload", forbidden)
    workload = load_workload(workload_path)
    if args.host_label is not None:
        require_safe_identifier(args.host_label, "host label")
    tree_capability = process_tree_capability()
    if tree_capability["state"] != "supported_best_effort":
        raise SafetyError(
            "workload execution requires verifiable stdlib process-group cleanup; "
            f"this platform reports {tree_capability['state']}"
        )
    input_root = guard_path(args.input, "input", forbidden)
    input_root = validate_safe_tree(input_root, "input")
    output_candidate = guard_path(args.output, "output", forbidden)
    require_disjoint_roots(input_root, output_candidate)
    input_filesystem = reject_network_filesystem(input_root, "input")
    output_filesystem = reject_network_filesystem(output_candidate.parent, "output")
    reject_network_filesystem(workload_path, "workload")

    current_platform = normalized_platform_name()
    platform_config = workload.get("platforms") or {}
    required_platforms = list(platform_config.get("required") or [current_platform])
    if current_platform not in required_platforms:
        raise ConfigError(
            f"workload does not admit normalized platform {current_platform!r}; "
            f"required={required_platforms}"
        )

    only_requested = args.only is not None
    only = set(args.only or [])
    unknown = only - {phase["name"] for phase in workload["phases"]}
    if unknown:
        raise ConfigError(f"--only references unknown phases {sorted(unknown)}")
    phases = [
        phase for phase in workload["phases"] if not only or phase["name"] in only
    ]
    # Pending product steps fail before any output root is created unless the
    # operator explicitly requests a not-evidence record.
    if not args.allow_pending:
        for phase in phases:
            reason = effective_phase_pending_reason(workload, phase)
            if reason is not None:
                raise ConfigError(
                    f"phase {phase['name']!r} is pending ({reason}); refusing to execute. "
                    "Re-run with --allow-pending to record it as not-evidence."
                )

    safety_cfg = workload.get("safety", {})
    # Validate declarations now, before creating any output.  Protected roots
    # are rejected; each run gets fixed runner-owned locations instead.
    build_child_env(
        dict(os.environ),
        dict(safety_cfg.get("env") or {}),
        list(safety_cfg.get("env_path_keys") or []),
        forbidden,
    )

    input_fingerprint = fingerprint_tree(input_root, "input corpus")
    binary = args.binary or workload.get("binary")
    if binary:
        binary_path = guard_path(binary, "binary", forbidden)
        reject_network_filesystem(binary_path, "binary")
        binary_identity(binary_path)
        binary = str(binary_path)
    frozen_ref = workload.get("frozen_identity", {})
    frozen_identity: dict[str, Any]
    identity_binding: dict[str, Any]
    config_source: Path | None = None
    bound_identity_document: dict[str, Any] | None = None
    bound_schema_manifest: Path | None = None
    if args.frozen_identity:
        identity_path = guard_path(args.frozen_identity, "frozen identity", forbidden)
        reject_network_filesystem(identity_path, "frozen identity")
        identity_path, identity = load_safe_json(identity_path, "frozen identity")
        if not binary or not args.schema_manifest or not args.config:
            raise ConfigError(
                "a frozen identity requires --binary, --schema-manifest, and --config "
                "to bind all tested artifacts"
            )
        schema_manifest_path = guard_path(args.schema_manifest, "schema manifest", forbidden)
        config_path = guard_path(args.config, "config", forbidden)
        reject_network_filesystem(schema_manifest_path, "schema manifest")
        reject_network_filesystem(config_path, "config")
        identity_binding = bind_frozen_identity(
            identity,
            binary_path=guard_path(binary, "binary", forbidden),
            schema_manifest_path=schema_manifest_path,
            workload_path=workload_path,
            corpus_root=input_root,
            config_path=config_path,
        )
        config_source = config_path
        bound_identity_document = identity
        bound_schema_manifest = schema_manifest_path
        frozen_identity = {
            "status": "supplied",
            "basename": identity_path.name,
            "sha256": sha256_file(identity_path, "frozen identity"),
            "schema_version": identity["schema_version"],
        }
    elif frozen_ref.get("required_for_evidence"):
        raise ConfigError(
            "workload requires a frozen identity artifact; supply --frozen-identity "
            "with --binary, --schema-manifest, and --config"
        )
    else:
        frozen_identity = {"status": "not_supplied"}
        identity_binding = {
            "status": "not_bound",
            "reason": "no frozen identity was supplied; this result is not evidence",
        }

    output_root = prepare_output_dir(args.output, forbidden)

    ctx = RunContext(
        workload=workload,
        input_root=input_root,
        output_root=output_root,
        base_env=dict(os.environ),
        forbidden=forbidden,
        timeout_default=float(workload.get("defaults", {}).get("timeout_seconds", 60.0)),
        binary=binary,
        config_source=config_source,
        bound_corpus=(
            identity_binding.get("components", {}).get("corpus")
            if identity_binding["status"] == "bound"
            else None
        ),
        bound_binary=(
            identity_binding.get("components", {}).get("binary")
            if identity_binding["status"] == "bound"
            else None
        ),
        bound_config=(
            identity_binding.get("components", {}).get("config")
            if identity_binding["status"] == "bound"
            else None
        ),
    )

    execution_failures: list[dict[str, str]] = []
    for phase in phases:
        for family in phase["families"]:
            try:
                execute_phase_for_family(ctx, phase, family, args.allow_pending)
            except ExecutionError as exc:
                # Preserve a terminal, explicitly non-evidence record without
                # serializing potentially sensitive child stdout/stderr.
                ctx.runs.append(
                    {
                        "phase": phase["name"],
                        "family": family,
                        "kind": phase["kind"],
                        "status": "failed",
                        "failure_class": type(exc).__name__,
                    }
                )
                execution_failures.append(
                    {"phase": phase["name"], "family": family, "class": type(exc).__name__}
                )

    if bound_identity_document is not None:
        # Re-read every bound artifact before publishing.  This detects an
        # external mutation after preflight and ensures the final result cannot
        # claim a freeze identity that differs from what its child processes saw.
        if config_source is None or bound_schema_manifest is None or not binary:
            raise SafetyError("bound identity state was lost before publication")
        identity_binding = bind_frozen_identity(
            bound_identity_document,
            binary_path=binary,
            schema_manifest_path=bound_schema_manifest,
            workload_path=workload_path,
            corpus_root=input_root,
            config_path=config_source,
        )

    environment = capture_environment(
        workload,
        args.host_label,
        bool(args.record_hostname),
        create_fresh_directory(output_root / "environment-probe", "environment probe"),
        forbidden,
    )
    # Commands are allowed only to mutate their own copy/sandbox.  Scan the
    # entire runner-owned output before publication so links, special files,
    # and hardlinks created by a child cannot become benchmark artifacts.
    validate_safe_tree(output_root, "runner output")

    limitations = list(workload.get("limitations") or [])
    not_evidence_reasons: list[str] = []
    pending = any(run.get("status") == "pending" for run in ctx.runs)
    if pending:
        not_evidence_reasons.append("one or more phases are pending and produced no measurements")
    if only_requested:
        not_evidence_reasons.append("--only was supplied; selected-phase output is partial")
    if execution_failures:
        not_evidence_reasons.append("one or more phase/family executions failed")
    if identity_binding["status"] != "bound":
        not_evidence_reasons.append("tested artifacts are not bound to a frozen identity")
    if not workload["evidence_eligible"]:
        not_evidence_reasons.append("workload is explicitly ineligible for product evidence")
    if input_filesystem["state"] != "local" or output_filesystem["state"] != "local":
        not_evidence_reasons.append(
            "input/output filesystem locality could not be verified"
        )
    if not_evidence_reasons:
        limitations.extend(not_evidence_reasons)
    scope_mode = "partial" if (only_requested or pending or execution_failures) else "full"
    evidence_state = "not_evidence" if not_evidence_reasons else "evidence"

    result = {
        "artifact_id": RESULT_ARTIFACT_ID,
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "completed" if evidence_state == "evidence" else "not_evidence",
        "evidence_status": {"state": evidence_state, "reasons": not_evidence_reasons},
        "execution_scope": {
            "mode": scope_mode,
            "only_requested": only_requested,
            "selected_phase_ids": [phase["name"] for phase in phases],
        },
        "workload": {
            "id": workload["workload_id"],
            "basename": workload_path.name,
            "sha256": sha256_file(workload_path, "workload"),
            "evidence_eligible": workload["evidence_eligible"],
        },
        "frozen_identity": frozen_identity,
        "identity_binding": identity_binding,
        "environment": environment,
        "platform": {
            "current": current_platform,
            "required": required_platforms,
            "configured_status": platform_config.get("status", {}),
            "enforcement": "current platform is a normalized required platform",
        },
        "process_tree_control": tree_capability,
        "safety": {
            "live_profile_guard": "enforced",
            "forbidden_roots_checked": [label for label, _ in forbidden],
            "child_env_scrubbed_prefixes": list(SCRUB_ENV_PREFIXES),
            "child_env_scrubbed_exact": list(SCRUB_ENV_EXACT),
            "recursive_lstat_tree_guard": "enforced",
            "unsafe_hardlinks": "rejected",
            "input_output_disjoint": "enforced",
            "fresh_runner_owned_store_copy_per_run": "enforced",
            "output_publication": "create_new_no_follow_atomic_link",
            "input_filesystem": input_filesystem,
            "output_filesystem": output_filesystem,
            "input_fingerprint_basis": "relative paths and SHA-256 only",
        },
        "logical_evidence_schema": LOGICAL_SQLITE_EVIDENCE_SCHEMA,
        "input_fingerprint": {
            "file_count": input_fingerprint["file_count"],
            "aggregate_sha256": input_fingerprint["aggregate_sha256"],
        },
        "runs": ctx.runs,
        "limitations": limitations,
    }

    problems = validate_result(result)
    absolute_hits = result_contains_absolute_paths(result)
    if absolute_hits:
        raise SafetyError(
            f"absolute paths leaked into result at {absolute_hits}; refusing publication"
        )
    if problems:
        result["status"] = "failed_validation"
        result["evidence_status"] = {
            "state": "not_evidence",
            "reasons": [*not_evidence_reasons, "result validation failed"],
        }
    if problems:
        result["validation_problems"] = problems

    result_path = output_root / "storage-runtime-baseline-result.json"
    atomic_write_new(
        result_path, json.dumps(result, indent=2, sort_keys=True) + "\n", "baseline result"
    )
    print(f"[s0] result written to {result_path}", file=sys.stderr)
    if problems:
        for problem in problems:
            print(f"[s0] validation problem: {problem}", file=sys.stderr)
        return 2
    return 2 if execution_failures else 0


def cmd_validate(args: argparse.Namespace) -> int:
    result_path, result = load_safe_json(args.result, "result artifact")
    problems = validate_result(result)
    problems.extend(
        f"absolute path leaked at {hit}"
        for hit in result_contains_absolute_paths(result)
    )
    if problems:
        for problem in problems:
            print(f"invalid: {problem}", file=sys.stderr)
        return 2
    print(f"valid: {result_path}")
    return 0


def cmd_self_test(args: argparse.Namespace) -> int:
    del args
    here = Path(__file__).resolve().parent
    workload_path = here / "workload-dry-run.json"
    fixture_src = here / "fixtures" / "dry-run-input"
    if not workload_path.is_file() or not fixture_src.is_dir():
        raise ConfigError("dry-run workload or fixture directory is missing")

    failures: list[str] = []

    def check(condition: bool, message: str) -> None:
        if not condition:
            failures.append(message)
        print(f"[self-test] {'PASS' if condition else 'FAIL'}: {message}")

    with tempfile.TemporaryDirectory(prefix="tracedecay-s0-selftest-") as tmp:
        tmp_root = Path(tmp)
        input_dir = tmp_root / "input"
        shutil.copytree(fixture_src, input_dir)
        output_dir = tmp_root / "output"

        # 1. Guard refuses a live-profile aliased output directory.
        fake_live = tmp_root / "fake-home" / ".tracedecay"
        fake_live.mkdir(parents=True)
        env = dict(os.environ)
        env["TRACEDECAY_DATA_DIR"] = str(fake_live)
        forbidden = forbidden_profile_roots(env, tmp_root / "fake-home")
        refused = False
        try:
            prepare_output_dir(fake_live / "nested", forbidden)
        except SafetyError:
            refused = True
        check(refused, "guard refuses output inside a live profile location")

        alias = tmp_root / "alias-to-live"
        try:
            alias.symlink_to(fake_live)
            refused = False
            try:
                prepare_output_dir(alias, forbidden)
            except SafetyError:
                refused = True
            check(refused, "guard refuses a symlink alias of a live profile location")
        except OSError:
            check(True, "symlink unsupported on this platform; alias check skipped")

        # 2. Child environment never inherits TraceDecay discovery variables.
        env["TRACEDECAY_GLOBAL_DB"] = str(fake_live / "global.db")
        child_env = build_child_env(env, {}, [], forbidden)
        check(
            not any(key.startswith("TRACEDECAY_") for key in child_env),
            "child environment strips TRACEDECAY_* variables",
        )

        # 3. Full dry-run execution end to end.
        rc = main(
            [
                "run",
                "--workload",
                str(workload_path),
                "--input",
                str(input_dir),
                "--output",
                str(output_dir),
            ]
        )
        check(rc == 0, f"dry-run workload executes cleanly (rc={rc})")

        result_path = output_dir / "storage-runtime-baseline-result.json"
        check(result_path.is_file(), "result artifact was written")
        if result_path.is_file():
            result = json.loads(result_path.read_text())
            problems = validate_result(result)
            check(not problems, f"result validates ({problems})")
            leaks = result_contains_absolute_paths(result)
            check(not leaks, f"result contains no absolute paths ({leaks})")
            check(
                result["status"] == "not_evidence"
                and result["evidence_status"]["state"] == "not_evidence",
                "dry-run output is explicitly not-evidence",
            )
            phases_run = {run["phase"] for run in result["runs"]}
            expected = {
                "current",
                "ten_x",
                "overload",
                "crash",
                "recovery",
                "fts",
                "backup_restore",
                "aa_noise",
            }
            check(
                expected <= phases_run,
                f"all dry-run phases executed (missing {sorted(expected - phases_run)})",
            )
            aa_runs = [
                run
                for run in result["runs"]
                if run["phase"] == "aa_noise" and run.get("aa")
            ]
            check(bool(aa_runs), "A/A noise-floor analysis recorded")
            if aa_runs:
                floor = aa_runs[0]["aa"]["noise_floor"]["p50_response_ns"]
                check(
                    floor["regression_margin_relative"] is not None,
                    "A/A regression margin computed",
                )

        # 4. Pending product steps fail closed without --allow-pending.
        pending_workload = tmp_root / "pending-workload.json"
        pending_doc = json.loads(workload_path.read_text())
        pending_doc["phases"][0]["work"]["argv"] = None
        pending_workload.write_text(json.dumps(pending_doc))
        output_dir2 = tmp_root / "output2"
        rc_pending = main(
            [
                "run",
                "--workload",
                str(pending_workload),
                "--input",
                str(input_dir),
                "--output",
                str(output_dir2),
            ]
        )
        check(
            rc_pending == 2 and not (output_dir2 / "storage-runtime-baseline-result.json").exists(),
            "pending steps fail closed without --allow-pending",
        )

    if failures:
        print(f"[self-test] {len(failures)} failure(s)", file=sys.stderr)
        return 1
    print("[self-test] all checks passed", file=sys.stderr)
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="TraceDecay SQLite storage runtime S0 baseline harness"
    )
    sub = parser.add_subparsers(dest="command", required=True)

    freeze = sub.add_parser(
        "freeze", help="capture the frozen released binary/schema identity"
    )
    freeze.add_argument("--binary", required=True, help="released binary to hash")
    freeze.add_argument(
        "--binary-version-argv",
        nargs="*",
        default=["--version"],
        help="argv appended to --binary to capture a version line",
    )
    freeze.add_argument(
        "--schema-manifest",
        required=True,
        help="released schema manifest file to hash (operator-supplied)",
    )
    freeze.add_argument(
        "--workload",
        required=True,
        help="exact workload JSON whose SHA-256 is frozen",
    )
    freeze.add_argument(
        "--corpus",
        required=True,
        help="exact safe corpus tree whose fingerprint is frozen",
    )
    freeze.add_argument(
        "--config",
        required=True,
        help="exact runtime configuration file/tree whose fingerprint is frozen",
    )
    freeze.add_argument("--output", required=True, help="identity artifact path")
    freeze.add_argument(
        "--store-family",
        action="append",
        default=[],
        help="supported store family (repeatable)",
    )
    freeze.add_argument("--notes", default="")
    freeze.set_defaults(func=cmd_freeze)

    run = sub.add_parser("run", help="execute a baseline workload")
    run.add_argument("--workload", required=True, help="workload JSON path")
    run.add_argument(
        "--input",
        required=True,
        help="explicit fixture/copy input directory (never the live profile)",
    )
    run.add_argument(
        "--output",
        required=True,
        help="fresh isolated output directory (must not already exist)",
    )
    run.add_argument("--binary", default=None, help="explicit binary under test")
    run.add_argument(
        "--schema-manifest",
        default=None,
        help="schema manifest to bind against --frozen-identity",
    )
    run.add_argument(
        "--config",
        default=None,
        help="runtime config file/tree to bind against --frozen-identity",
    )
    run.add_argument("--frozen-identity", default=None, help="freeze artifact path")
    run.add_argument(
        "--allow-pending",
        action="store_true",
        help="record pending phases as not run instead of failing closed",
    )
    run.add_argument("--only", nargs="*", default=None, help="restrict to phases")
    run.add_argument("--host-label", default=None, help="stable host label")
    run.add_argument(
        "--record-hostname",
        action="store_true",
        help="record the hostname (default redacts it)",
    )
    run.set_defaults(func=cmd_run)

    validate = sub.add_parser("validate", help="validate a result artifact")
    validate.add_argument("--result", required=True)
    validate.set_defaults(func=cmd_validate)

    self_test = sub.add_parser(
        "self-test", help="run the checked-in dry-run end to end with assertions"
    )
    self_test.set_defaults(func=cmd_self_test)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except RunnerError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
