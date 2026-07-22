"""Race-resistant path validation, hashing, copying, and publication."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import stat
import uuid
from pathlib import Path
from typing import Any

from runner_contract import SAFE_IDENTIFIER, SafetyError


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


NodeIdentity = tuple[int, int, int, int]
TreeSnapshot = dict[Path, NodeIdentity]


def _node_identity(info: os.stat_result) -> NodeIdentity:
    return (
        int(info.st_dev),
        int(info.st_ino),
        int(stat.S_IFMT(info.st_mode)),
        int(getattr(info, "st_nlink", 1)),
    )


def _require_snapshot_identity(
    path: Path, info: os.stat_result, expected: NodeIdentity, role: str
) -> None:
    if _node_identity(info) != expected:
        raise SafetyError(f"{role} entry changed after validation: {path}")


def _scan_safe_directory(
    directory: Path, role: str, expected: NodeIdentity | None = None
) -> tuple[os.stat_result, list[os.DirEntry]]:
    """Read one directory without accepting a replacement during the scan."""
    assert_safe_path_components(directory, role, require_directory=True)
    try:
        before = os.lstat(directory)
        _reject_unsafe_node(directory, before, role)
        if expected is not None:
            _require_snapshot_identity(directory, before, expected, role)
        with os.scandir(directory) as iterator:
            entries = sorted(iterator, key=lambda entry: entry.name)
        after = os.lstat(directory)
    except OSError as exc:
        raise SafetyError(f"cannot scan {role} directory {directory}: {exc}") from exc
    _reject_unsafe_node(directory, after, role)
    _require_snapshot_identity(directory, after, expected or _node_identity(before), role)
    return after, entries


def _require_snapshot_children(
    directory: Path,
    entries: list[os.DirEntry],
    expected: dict[str, NodeIdentity],
    role: str,
) -> None:
    if {entry.name for entry in entries} != expected.keys():
        raise SafetyError(f"{role} directory changed: {directory}")
    for name, identity in expected.items():
        child = directory / name
        try:
            info = os.lstat(child)
        except OSError as exc:
            raise SafetyError(f"cannot recheck {role} entry {child}: {exc}") from exc
        _reject_unsafe_node(child, info, role)
        _require_snapshot_identity(child, info, identity, role)


def snapshot_safe_tree(root_like: str | Path, role: str) -> tuple[Path, TreeSnapshot]:
    """Recursively lstat a tree and retain every accepted object's identity.

    ``os.walk`` is insufficient because it silently skips links and accepts
    special nodes. A snapshot lets a later copy reject an inode replacement,
    rather than treating a safe-looking replacement as the original fixture.
    """
    root = assert_safe_path_components(root_like, role, require_directory=True)
    snapshot: TreeSnapshot = {}

    def remember(relative: Path, info: os.stat_result, path: Path) -> None:
        identity = _node_identity(info)
        existing = snapshot.get(relative)
        if existing is not None and existing != identity:
            raise SafetyError(f"{role} entry changed while scanning: {path}")
        snapshot[relative] = identity

    def visit(directory: Path, relative: Path) -> None:
        info, entries = _scan_safe_directory(directory, role)
        remember(relative, info, directory)
        for entry in entries:
            child = directory / entry.name
            child_relative = relative / entry.name
            try:
                info = os.lstat(child)
            except OSError as exc:
                raise SafetyError(f"cannot lstat {role} entry {child}: {exc}") from exc
            _reject_unsafe_node(child, info, role)
            remember(child_relative, info, child)
            if stat.S_ISDIR(info.st_mode):
                visit(child, child_relative)
        _final, final_entries = _scan_safe_directory(directory, role, snapshot[relative])
        _require_snapshot_children(
            directory,
            final_entries,
            {entry.name: snapshot[relative / entry.name] for entry in entries},
            role,
        )

    visit(root, Path("."))
    return root, snapshot


def assert_safe_tree_snapshot(
    root_like: str | Path, expected: TreeSnapshot, role: str
) -> Path:
    """Refuse a tree whose nodes differ from a previously accepted snapshot."""
    root, current = snapshot_safe_tree(root_like, role)
    if current != expected:
        raise SafetyError(f"{role} changed after validation")
    return root


def validate_safe_tree(root_like: str | Path, role: str) -> Path:
    """Recursively lstat a tree before it becomes a benchmark input."""
    root, _snapshot = snapshot_safe_tree(root_like, role)
    return root


def _open_read_no_follow(
    path: Path,
    role: str,
    expected_identity: NodeIdentity | None = None,
) -> int:
    """Open one validated regular file and re-check identity after open."""
    assert_safe_path_components(path, role, require_directory=False)
    before = os.lstat(path)
    _reject_unsafe_node(path, before, role)
    if expected_identity is not None:
        _require_snapshot_identity(path, before, expected_identity, role)
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
        if _node_identity(before) != _node_identity(after):
            raise SafetyError(f"{role} file changed while opening: {path}")
        if expected_identity is not None:
            _require_snapshot_identity(path, after, expected_identity, role)
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def read_file_no_follow(
    path_like: str | Path,
    role: str,
    *,
    max_bytes: int,
) -> bytes:
    """Read one regular file without following links or exceeding a byte bound."""
    if isinstance(max_bytes, bool) or not isinstance(max_bytes, int) or max_bytes < 0:
        raise SafetyError(f"{role} byte bound must be a non-negative integer")
    path = assert_safe_path_components(path_like, role, require_directory=False)
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise SafetyError(f"cannot lstat {role} file {path}: {exc}") from exc
    if before.st_size > max_bytes:
        raise SafetyError(f"{role} exceeds artifact size bound")

    descriptor = _open_read_no_follow(path, role, _node_identity(before))
    try:
        with os.fdopen(descriptor, "rb") as handle:
            data = handle.read(max_bytes + 1)
            after = os.fstat(handle.fileno())
    except OSError as exc:
        raise SafetyError(f"cannot safely read {role} file {path}: {exc}") from exc
    if len(data) > max_bytes or len(data) != after.st_size:
        raise SafetyError(f"{role} changed or exceeded bounds while reading")
    _require_snapshot_identity(path, after, _node_identity(before), role)
    if before.st_size != after.st_size:
        raise SafetyError(f"{role} changed while reading")
    return data


def canonical_compact_json(document: Any, *, ensure_ascii: bool = False) -> str:
    """Serialize deterministic compact JSON for hashing and wire artifacts."""
    return json.dumps(
        document,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=ensure_ascii,
    )


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path, role: str = "hashed") -> str:
    digest = hashlib.sha256()
    descriptor = _open_read_no_follow(path, role)
    with os.fdopen(descriptor, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_text(text: str) -> str:
    return sha256_bytes(text.encode("utf-8"))


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
    aggregate = sha256_text(canonical_compact_json(entries))
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


def atomic_write_json_new(
    path_like: str | Path,
    document: Any,
    role: str,
    *,
    indent: int | None = None,
    ensure_ascii: bool = False,
) -> Path:
    """Serialize deterministic JSON and publish it as a new no-replace file."""
    if indent is None:
        data = canonical_compact_json(document, ensure_ascii=ensure_ascii)
    else:
        data = json.dumps(
            document,
            indent=indent,
            sort_keys=True,
            ensure_ascii=ensure_ascii,
        )
    return atomic_write_new(path_like, data + "\n", role)


def copy_safe_file(
    source: Path,
    destination: Path,
    role: str,
    expected_identity: NodeIdentity | None = None,
) -> None:
    """Copy a single validated regular file into a fresh no-follow destination."""
    source_descriptor = _open_read_no_follow(source, role, expected_identity)
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


def copy_safe_tree(
    source_like: str | Path,
    destination_like: str | Path,
    role: str,
    *,
    source_snapshot: TreeSnapshot | None = None,
) -> Path:
    """Make a fully independent, runner-owned store copy for exactly one run.

    Copy only objects that still match the lstat snapshot.  This closes the
    preflight-to-copy replacement window that would otherwise allow a fixture
    directory to be swapped for another tree after validation.
    """
    source_role = f"{role} source"
    if source_snapshot is None:
        source, source_snapshot = snapshot_safe_tree(source_like, source_role)
    else:
        source = assert_safe_tree_snapshot(source_like, source_snapshot, source_role)
    destination = create_fresh_directory(destination_like, f"{role} destination")
    children_by_parent: dict[Path, dict[str, NodeIdentity]] = {}
    for child, identity in source_snapshot.items():
        if child != Path("."):
            children_by_parent.setdefault(child.parent, {})[child.name] = identity

    def copy_directory(source_dir: Path, destination_dir: Path, relative: Path) -> None:
        expected = source_snapshot.get(relative)
        if expected is None:
            raise SafetyError(f"{role} source is missing snapshot identity: {source_dir}")
        _info, entries = _scan_safe_directory(source_dir, source_role, expected)
        expected_children = children_by_parent.get(relative, {})
        _require_snapshot_children(source_dir, entries, expected_children, source_role)
        for entry in entries:
            source_child = source_dir / entry.name
            destination_child = destination_dir / entry.name
            child_relative = relative / entry.name
            expected_child = source_snapshot.get(child_relative)
            if expected_child is None:
                raise SafetyError(f"{role} source is missing snapshot identity: {source_child}")
            try:
                info = os.lstat(source_child)
            except OSError as exc:
                raise SafetyError(f"cannot lstat {role} source entry {source_child}: {exc}") from exc
            _reject_unsafe_node(source_child, info, role)
            _require_snapshot_identity(source_child, info, expected_child, role)
            if stat.S_ISDIR(info.st_mode):
                child_dir = create_fresh_directory(destination_child, f"{role} copy")
                copy_directory(source_child, child_dir, child_relative)
            else:
                copy_safe_file(source_child, destination_child, role, expected_child)

    copy_directory(source, destination, Path("."))
    assert_safe_tree_snapshot(source, source_snapshot, source_role)
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
