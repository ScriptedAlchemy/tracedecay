"""Deterministic, disposable fixtures for runtime regression samples."""

from __future__ import annotations

import hashlib
import json
import os
import platform as platform_module
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping


ISOLATED_ENVIRONMENT_KEYS = (
    "HOME",
    "USERPROFILE",
    "XDG_CONFIG_HOME",
    "TRACEDECAY_DATA_DIR",
    "TRACEDECAY_GLOBAL_DB",
    "TRACEDECAY_DAEMON_SOCKET",
)
REMOVED_OPERATOR_PROFILE_KEYS = (
    "TRACEDECAY_HOME",
    "TRACEDECAY_PROFILE",
    "TRACEDECAY_PROFILE_DIR",
)
RUNTIME_STATES = frozenset({"cold", "warm", "no-op", "contention", "recovery"})
FIXTURE_ID = "runtime-v2-final"

_PROVIDER_RELATIVE_FILES = {
    "codex": Path(".codex/sessions/2026/07/fixture-session.jsonl"),
    "claude": Path(
        ".claude/projects/-workspace-runtime-fixture/fixture-session.jsonl"
    ),
    "cursor": Path(
        ".cursor/projects/fixture-runtime-project/"
        "agent-transcripts/fixture-session.jsonl"
    ),
}


class FixtureError(RuntimeError):
    """Raised when a fixture cannot be prepared without leaking host state."""


@dataclass(frozen=True)
class PreparedFixture:
    """Paths and immutable provenance for one disposable profile snapshot."""

    snapshot_root: Path
    home: Path
    project: Path
    provider_roots: dict[str, Path]
    provider_files: dict[str, Path]
    prebuilt_binary: Path
    evidence_root: Path
    prepared_evidence: Path
    runtime_identity: dict[str, str | int]
    environment: dict[str, str]
    fixture_digests: dict[str, str]
    git_head: str
    sample_count: int = 1
    measurement_class: str = "n=1 regression sample"


def fixture_source_root() -> Path:
    """Return the checked-in deterministic fixture root."""

    return Path(__file__).resolve().with_name("fixtures")


def provider_roots(home: Path) -> dict[str, Path]:
    """Return native provider roots inside an isolated home."""

    return {
        "codex": home / ".codex",
        "claude": home / ".claude",
        "cursor": home / ".cursor",
    }


def provider_fixture_files(root: Path) -> dict[str, Path]:
    """Return the canonical provider transcript files below a fixture root."""

    source_home = root / "providers" / "home"
    if not source_home.is_dir():
        source_home = root
    return {
        provider: source_home / relative
        for provider, relative in _PROVIDER_RELATIVE_FILES.items()
    }


def copy_fixture_source(source: Path, destination: Path) -> None:
    """Copy a fixture tree after rejecting every symlink."""

    source = Path(source)
    destination = Path(destination)
    _validate_plain_tree(source)
    if destination.exists():
        raise FixtureError(f"fixture destination already exists: {destination}")
    shutil.copytree(source, destination, copy_function=shutil.copy2)
    _validate_plain_tree(destination)


def prepare_fixture_snapshot(
    snapshot_root: Path,
    *,
    prebuilt_binary: Path,
    fixture_root: Path | None = None,
    platform: str | None = None,
    shard: str = "integrated-0",
    storage_mode: str = "isolated-sqlite",
    concurrency: int = 1,
    runtime_state: str = "cold",
    temperature: str | None = None,
) -> PreparedFixture:
    """Prepare one isolated project/profile snapshot from checked-in bytes."""

    source = Path(fixture_root) if fixture_root is not None else fixture_source_root()
    binary = Path(prebuilt_binary)
    _validate_prebuilt_binary(binary)
    _validate_plain_tree(source)
    runtime_identity = _runtime_identity(
        platform=platform,
        shard=shard,
        storage_mode=storage_mode,
        concurrency=concurrency,
        runtime_state=runtime_state,
        temperature=temperature,
    )

    snapshot_root = Path(snapshot_root)
    if snapshot_root.exists() and any(snapshot_root.iterdir()):
        raise FixtureError(f"snapshot root is not empty: {snapshot_root}")
    snapshot_root.mkdir(parents=True, exist_ok=True)

    home = snapshot_root / "home"
    project = home / "workspace" / "runtime-fixture"
    project.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source / "project", project, copy_function=shutil.copy2)
    shutil.copytree(
        source / "providers" / "home",
        home,
        dirs_exist_ok=True,
        copy_function=shutil.copy2,
    )

    (home / ".config").mkdir(parents=True)
    (snapshot_root / "data" / "tracedecay").mkdir(parents=True)
    (snapshot_root / "run").mkdir(parents=True)
    copied_binary = snapshot_root / "bin" / "tracedecay"
    copied_binary.parent.mkdir(parents=True)
    shutil.copy2(binary, copied_binary)
    copied_binary.chmod(binary.stat().st_mode & 0o777)

    evidence_root = snapshot_root / "evidence"
    evidence_root.mkdir()
    shutil.copy2(source / "metadata.json", evidence_root / "metadata.json")
    shutil.copytree(
        source / "measurements",
        evidence_root / "measurements",
        copy_function=shutil.copy2,
    )
    shutil.copytree(
        source / "raw_samples",
        evidence_root / "raw_samples",
        copy_function=shutil.copy2,
    )
    prepared_evidence = evidence_root / "prepared.json"
    _write_prepared_evidence(prepared_evidence, runtime_identity)

    _initialize_deterministic_git_history(project, home)
    git_head = _git_output(project, home, "rev-parse", "HEAD").strip()
    prepared = PreparedFixture(
        snapshot_root=snapshot_root,
        home=home,
        project=project,
        provider_roots=provider_roots(home),
        provider_files=provider_fixture_files(home),
        prebuilt_binary=copied_binary,
        evidence_root=evidence_root,
        prepared_evidence=prepared_evidence,
        runtime_identity=runtime_identity,
        environment={},
        fixture_digests=_snapshot_digests(snapshot_root),
        git_head=git_head,
    )
    return PreparedFixture(
        **{
            **prepared.__dict__,
            "environment": isolated_environment(prepared),
        }
    )


def clone_prepared_profile(
    prepared: PreparedFixture,
    destination: Path,
    *,
    prebuilt_binary: Path | None = None,
    runtime_state: str | None = None,
    temperature: str | None = None,
) -> PreparedFixture:
    """Clone a prepared snapshot without sharing files or mutable state."""

    destination = Path(destination)
    if destination.exists():
        raise FixtureError(f"clone destination already exists: {destination}")
    shutil.copytree(
        prepared.snapshot_root,
        destination,
        copy_function=shutil.copy2,
    )
    copied_binary = destination / "bin" / "tracedecay"
    if prebuilt_binary is not None:
        binary = Path(prebuilt_binary)
        _validate_prebuilt_binary(binary)
        shutil.copy2(binary, copied_binary)
        copied_binary.chmod(binary.stat().st_mode & 0o777)
    home = destination / "home"
    runtime_identity = dict(prepared.runtime_identity)
    if runtime_state is not None:
        if runtime_state not in RUNTIME_STATES:
            raise FixtureError(f"unsupported runtime state: {runtime_state}")
        runtime_identity["runtime_state"] = runtime_state
    resolved_temperature = temperature
    if resolved_temperature is None and runtime_state is not None:
        resolved_temperature = "cold" if runtime_state == "cold" else "warm"
    if resolved_temperature is not None:
        if resolved_temperature not in {"cold", "warm"}:
            raise FixtureError(
                f"unsupported runtime temperature: {resolved_temperature}"
            )
        runtime_identity["temperature"] = resolved_temperature
    prepared_evidence = destination / "evidence" / "prepared.json"
    _write_prepared_evidence(prepared_evidence, runtime_identity)
    clone = PreparedFixture(
        snapshot_root=destination,
        home=home,
        project=home / "workspace" / "runtime-fixture",
        provider_roots=provider_roots(home),
        provider_files=provider_fixture_files(home),
        prebuilt_binary=copied_binary,
        evidence_root=destination / "evidence",
        prepared_evidence=prepared_evidence,
        runtime_identity=runtime_identity,
        environment={},
        fixture_digests=_snapshot_digests(destination),
        git_head=prepared.git_head,
    )
    return PreparedFixture(
        **{
            **clone.__dict__,
            "environment": isolated_environment(clone),
        }
    )


def isolated_environment(
    prepared: PreparedFixture,
    *,
    base: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Return an inherited environment with every profile path rebased."""

    environment = dict(os.environ if base is None else base)
    for key in REMOVED_OPERATOR_PROFILE_KEYS:
        environment.pop(key, None)
    data_dir = prepared.snapshot_root / "data" / "tracedecay"
    environment.update(
        {
            "HOME": str(prepared.home),
            "USERPROFILE": str(prepared.home),
            "XDG_CONFIG_HOME": str(prepared.home / ".config"),
            "TRACEDECAY_DATA_DIR": str(data_dir),
            "TRACEDECAY_GLOBAL_DB": str(data_dir / "global.db"),
            "TRACEDECAY_DAEMON_SOCKET": str(
                prepared.snapshot_root / "run" / "tracedecay.sock"
            ),
        }
    )
    return environment


def _runtime_identity(
    *,
    platform: str | None,
    shard: str,
    storage_mode: str,
    concurrency: int,
    runtime_state: str,
    temperature: str | None,
) -> dict[str, str | int]:
    resolved_platform = platform or (
        f"{sys.platform}-{platform_module.machine().lower() or 'unknown'}"
    )
    string_fields = {
        "platform": resolved_platform,
        "shard": shard,
        "storage_mode": storage_mode,
    }
    for field, value in string_fields.items():
        if not value:
            raise FixtureError(f"runtime identity {field} must not be empty")
    if concurrency < 1:
        raise FixtureError("runtime identity concurrency must be positive")
    if runtime_state not in RUNTIME_STATES:
        raise FixtureError(f"unsupported runtime state: {runtime_state}")
    resolved_temperature = temperature or (
        "cold" if runtime_state == "cold" else "warm"
    )
    if resolved_temperature not in {"cold", "warm"}:
        raise FixtureError(f"unsupported runtime temperature: {resolved_temperature}")
    return {
        "fixture_id": FIXTURE_ID,
        **string_fields,
        "concurrency": concurrency,
        "runtime_state": runtime_state,
        "temperature": resolved_temperature,
    }


def _write_prepared_evidence(
    path: Path,
    runtime_identity: Mapping[str, str | int],
) -> None:
    document = {
        "schema_version": 1,
        "sample_count": 1,
        "measurement_class": "n=1 regression sample",
        "runtime_identity": dict(runtime_identity),
    }
    path.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def _validate_prebuilt_binary(binary: Path) -> None:
    if binary.is_symlink() or not binary.is_file():
        raise FixtureError(f"prebuilt binary is missing or not a plain file: {binary}")
    if not os.access(binary, os.X_OK):
        raise FixtureError(f"prebuilt binary is not executable: {binary}")


def _validate_plain_tree(root: Path) -> None:
    if root.is_symlink() or not root.is_dir():
        raise FixtureError(f"fixture source is not a plain directory: {root}")
    for path in root.rglob("*"):
        if path.is_symlink():
            raise FixtureError(f"fixture source contains symlink: {path}")


def _git_environment(home: Path) -> dict[str, str]:
    environment = dict(os.environ)
    environment.update(
        {
            "HOME": str(home),
            "USERPROFILE": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_AUTHOR_NAME": "TraceDecay Fixture",
            "GIT_AUTHOR_EMAIL": "fixture@tracedecay.invalid",
            "GIT_COMMITTER_NAME": "TraceDecay Fixture",
            "GIT_COMMITTER_EMAIL": "fixture@tracedecay.invalid",
            "GIT_AUTHOR_DATE": "2024-01-02T03:04:05+00:00",
            "GIT_COMMITTER_DATE": "2024-01-02T03:04:05+00:00",
        }
    )
    return environment


def _git_output(project: Path, home: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-C", str(project), *arguments],
        check=True,
        capture_output=True,
        text=True,
        env=_git_environment(home),
    ).stdout


def _initialize_deterministic_git_history(project: Path, home: Path) -> None:
    _git_output(project, home, "init", "-q", "-b", "main")
    _git_output(
        project,
        home,
        "add",
        "--",
        "README.md",
        "src/catalog.py",
        "src/graph.ts",
    )
    _git_output(
        project,
        home,
        "commit",
        "-q",
        "-m",
        "feat: add fixture catalog",
    )
    _git_output(project, home, "add", "--", ".")
    _git_output(
        project,
        home,
        "commit",
        "-q",
        "-m",
        "feat: add fixture report",
    )


def _snapshot_digests(snapshot_root: Path) -> dict[str, str]:
    digests: dict[str, str] = {}
    combined = hashlib.sha256()
    files = sorted(
        (
            path
            for path in snapshot_root.rglob("*")
            if path.is_file() and ".git" not in path.relative_to(snapshot_root).parts
        ),
        key=lambda path: path.relative_to(snapshot_root).as_posix(),
    )
    for path in files:
        relative = path.relative_to(snapshot_root).as_posix()
        content = path.read_bytes()
        digest = hashlib.sha256(content).hexdigest()
        digests[relative] = digest
        combined.update(relative.encode("utf-8"))
        combined.update(b"\0")
        combined.update(content)
        combined.update(b"\0")
    digests["combined"] = combined.hexdigest()
    return digests
