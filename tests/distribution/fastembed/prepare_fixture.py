#!/usr/bin/env python3
"""Acquire the immutable real FastEmbed fixture before offline acceptance."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
from urllib.parse import quote


SCHEMA = "tracedecay.distribution.fastembed-fixture.v1"
MODEL = "JinaEmbeddingsV2BaseCode"
REVISION = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")
MEMBERS = {
    "model": "model.onnx",
    "tokenizer": "tokenizer.json",
    "config": "config.json",
    "special_tokens_map": "special_tokens_map.json",
    "tokenizer_config": "tokenizer_config.json",
}
MAX_TOTAL_BYTES = 700 * 1024 * 1024


def fail(message: str) -> None:
    raise SystemExit(f"FastEmbed fixture preparation: {message}")


def read_manifest(source: pathlib.Path) -> dict[str, object]:
    manifest_path = source / "fixture.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        fail(f"missing regular source manifest {manifest_path}")
    try:
        document = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"cannot read {manifest_path}: {error}")
    if not isinstance(document, dict) or document.get("schema") != SCHEMA:
        fail(f"fixture.json schema must be {SCHEMA!r}")
    if document.get("model") != MODEL:
        fail(f"fixture.json model must be {MODEL!r}")

    source_metadata = document.get("source")
    if not isinstance(source_metadata, dict):
        fail("fixture.json source must be an object")
    upstream = source_metadata.get("upstream")
    revision = source_metadata.get("revision")
    if upstream != "https://huggingface.co/jinaai/jina-embeddings-v2-base-code":
        fail("fixture source must be the upstream Jina code-embedding repository")
    if not isinstance(revision, str) or REVISION.fullmatch(revision) is None:
        fail("fixture source revision must be a full lowercase Git commit")
    if source_metadata.get("license") != "Apache-2.0":
        fail("fixture source license must be Apache-2.0")
    if source_metadata.get("license_url") != "https://www.apache.org/licenses/LICENSE-2.0":
        fail("fixture source license_url must identify the Apache-2.0 license")
    provenance = source_metadata.get("provenance")
    if not isinstance(provenance, str) or revision not in provenance:
        fail("fixture source provenance must identify the immutable revision")

    if document.get("expected_dimensions") != 768:
        fail("JinaEmbeddingsV2BaseCode must declare 768 dimensions")
    max_length = document.get("max_length")
    if not isinstance(max_length, int) or isinstance(max_length, bool) or max_length != 8192:
        fail("JinaEmbeddingsV2BaseCode must declare its 8192-token maximum")

    members = document.get("members")
    if not isinstance(members, dict) or set(members) != set(MEMBERS):
        fail("fixture.json members must contain exactly: " + ", ".join(MEMBERS))
    total_length = 0
    for role, expected_name in MEMBERS.items():
        member = members[role]
        if not isinstance(member, dict) or member.get("path") != expected_name:
            fail(f"members.{role}.path must be {expected_name!r}")
        upstream_path = member.get("upstream_path")
        if (
            not isinstance(upstream_path, str)
            or not upstream_path
            or pathlib.PurePosixPath(upstream_path).is_absolute()
            or ".." in pathlib.PurePosixPath(upstream_path).parts
        ):
            fail(f"members.{role}.upstream_path must be a normalized relative path")
        length = member.get("length")
        if not isinstance(length, int) or isinstance(length, bool) or length <= 0:
            fail(f"members.{role}.length must be a positive integer")
        digest = member.get("sha256")
        if not isinstance(digest, str) or SHA256.fullmatch(digest) is None:
            fail(f"members.{role}.sha256 must be 64 lowercase hexadecimal characters")
        total_length += length
    if total_length > MAX_TOTAL_BYTES:
        fail(f"declared fixture size {total_length} exceeds {MAX_TOTAL_BYTES}")
    return document


def digest_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def member_is_valid(path: pathlib.Path, length: int, digest: str) -> bool:
    return (
        not path.is_symlink()
        and path.is_file()
        and path.stat().st_size == length
        and digest_file(path) == digest
    )


def acquire_member(
    stage: pathlib.Path,
    upstream: str,
    revision: str,
    role: str,
    member: dict[str, object],
) -> None:
    target = stage / str(member["path"])
    length = int(member["length"])
    digest = str(member["sha256"])
    if member_is_valid(target, length, digest):
        return
    if target.is_symlink():
        target.unlink()
    elif target.exists() and target.stat().st_size >= length:
        target.unlink()
    upstream_path = quote(str(member["upstream_path"]), safe="/")
    url = f"{upstream}/resolve/{revision}/{upstream_path}?download=true"
    command = [
        "curl",
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--retry",
        "3",
        "--retry-all-errors",
        "--connect-timeout",
        "30",
        "--continue-at",
        "-",
        "--output",
        str(target),
        url,
    ]
    try:
        subprocess.run(command, check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        fail(f"cannot acquire members.{role} from immutable upstream: {error}")
    if target.stat().st_size != length:
        fail(
            f"members.{role} length mismatch: expected {length}, "
            f"got {target.stat().st_size}"
        )
    actual_digest = digest_file(target)
    if actual_digest != digest:
        fail(
            f"members.{role} digest mismatch: expected {digest}, got {actual_digest}"
        )


def seed_stage_from_cache(
    stage: pathlib.Path,
    cache: pathlib.Path | None,
    members: dict[str, object],
) -> None:
    """Copy digest-valid members from a local cache before upstream curl.

    The cache is only a setup accelerator. Every member is still length- and
    digest-checked by acquire_member before the destination is published.
    """
    if cache is None:
        return
    if cache.is_symlink() or not cache.is_dir():
        fail(f"fixture cache must be a regular directory: {cache}")
    for role, expected_name in MEMBERS.items():
        member = members[role]
        assert isinstance(member, dict)
        cached = cache / expected_name
        target = stage / expected_name
        if not member_is_valid(cached, int(member["length"]), str(member["sha256"])):
            continue
        if target.exists() or target.is_symlink():
            target.unlink()
        shutil.copyfile(cached, target)


def prepare(source: pathlib.Path, destination: pathlib.Path) -> None:
    document = read_manifest(source)
    if destination.exists() or destination.is_symlink():
        fail(f"destination already exists: {destination}")
    stage = destination.with_name(destination.name + ".staging")
    if stage.is_symlink():
        fail(f"staging path must not be a symlink: {stage}")
    stage.mkdir(parents=True, exist_ok=True)
    if not stage.is_dir():
        fail(f"staging path is not a directory: {stage}")
    members = document["members"]
    source_metadata = document["source"]
    assert isinstance(members, dict)
    assert isinstance(source_metadata, dict)
    upstream = str(source_metadata["upstream"])
    revision = str(source_metadata["revision"])
    cache_raw = os.environ.get("TRACEDECAY_DISTRIBUTION_FASTEMBED_CACHE", "").strip()
    cache = pathlib.Path(cache_raw) if cache_raw else None
    seed_stage_from_cache(stage, cache, members)
    for role in MEMBERS:
        member = members[role]
        assert isinstance(member, dict)
        acquire_member(stage, upstream, revision, role, member)
    staged_manifest = stage / "fixture.json"
    if staged_manifest.is_symlink():
        staged_manifest.unlink()
    shutil.copyfile(source / "fixture.json", staged_manifest)
    os.replace(stage, destination)
    print(destination)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("source", type=pathlib.Path)
    parser.add_argument("destination", nargs="?", type=pathlib.Path)
    arguments = parser.parse_args()
    document = read_manifest(arguments.source)
    if arguments.check:
        if arguments.destination is not None:
            fail("--check does not accept a destination")
        print(f"{document['expected_dimensions']}\t{document['max_length']}")
        return
    if arguments.destination is None:
        fail("destination is required unless --check is used")
    prepare(arguments.source, arguments.destination)


if __name__ == "__main__":
    main()
