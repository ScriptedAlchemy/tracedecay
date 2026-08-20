#!/usr/bin/env python3
"""Prepare a checksum-pinned target runtime for release builds."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import tarfile
import urllib.request
from pathlib import Path


MAX_ARCHIVE_BYTES = 256 * 1024 * 1024


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--github-env", type=Path, required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    return parser.parse_args()


def append_lines(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8", newline="\n") as output:
        for line in lines:
            output.write(f"{line}\n")


def find_target(manifest: Path, name: str) -> dict[str, object]:
    document = json.loads(manifest.read_text(encoding="utf-8"))
    matches = [entry for entry in document["include"] if entry["name"] == name]
    if len(matches) != 1:
        raise ValueError(f"release target {name!r} must appear exactly once")
    return matches[0]


def download(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=60) as response:
        payload = response.read(MAX_ARCHIVE_BYTES + 1)
    if len(payload) > MAX_ARCHIVE_BYTES:
        raise ValueError("release runtime archive exceeds the size limit")
    return payload


def extract_runtime(payload: bytes, archive_entry: str) -> bytes:
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
        member = archive.getmember(archive_entry)
        if not member.isfile():
            raise ValueError(f"runtime archive entry is not a regular file: {archive_entry}")
        source = archive.extractfile(member)
        if source is None:
            raise ValueError(f"runtime archive entry could not be read: {archive_entry}")
        runtime = source.read(MAX_ARCHIVE_BYTES + 1)
    if not runtime:
        raise ValueError("release runtime library is empty")
    if len(runtime) > MAX_ARCHIVE_BYTES:
        raise ValueError("release runtime library exceeds the size limit")
    return runtime


def main() -> None:
    args = parse_args()
    target = find_target(args.manifest, args.name)
    runtime = target.get("runtime")
    if runtime is None:
        append_lines(
            args.github_output,
            ["runtime_library=", "runtime_entry_name="],
        )
        return
    if not isinstance(runtime, dict):
        raise ValueError("release target runtime metadata must be an object")

    url = str(runtime["url"])
    expected_sha256 = str(runtime["sha256"])
    archive_entry = str(runtime["archive_entry"])
    entry_name = str(runtime["entry_name"])
    link_name = str(runtime["link_name"])
    if Path(entry_name).name != entry_name or not entry_name:
        raise ValueError("runtime entry name must be a nonempty basename")
    if Path(link_name).name != link_name or not link_name:
        raise ValueError("runtime link name must be a nonempty basename")
    if link_name == entry_name:
        raise ValueError("runtime link and entry names must differ")
    if len(expected_sha256) != 64 or any(
        character not in "0123456789abcdef" for character in expected_sha256
    ):
        raise ValueError("runtime SHA-256 must be 64 lowercase hexadecimal digits")

    archive_payload = download(url)
    actual_sha256 = hashlib.sha256(archive_payload).hexdigest()
    if actual_sha256 != expected_sha256:
        raise ValueError(
            f"release runtime SHA-256 mismatch: got {actual_sha256}, "
            f"expected {expected_sha256}"
        )
    runtime_payload = extract_runtime(archive_payload, archive_entry)

    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    runtime_library = output / entry_name
    runtime_library.write_bytes(runtime_payload)
    runtime_library.chmod(0o644)
    (output / link_name).symlink_to(entry_name)

    ld_library_path = str(output)
    if current_ld_library_path := os.environ.get("LD_LIBRARY_PATH"):
        ld_library_path = f"{output}{os.pathsep}{current_ld_library_path}"
    rustflags = os.environ.get("RUSTFLAGS", "").strip()
    rustflags = f"{rustflags} -C link-arg=-Wl,-rpath,$ORIGIN".strip()
    append_lines(
        args.github_env,
        [
            f"ORT_LIB_PATH={output}",
            f"ORT_LIB_LOCATION={output}",
            "ORT_PREFER_DYNAMIC_LINK=1",
            f"LD_LIBRARY_PATH={ld_library_path}",
            f"RUSTFLAGS={rustflags}",
        ],
    )
    append_lines(
        args.github_output,
        [
            f"runtime_library={runtime_library}",
            f"runtime_entry_name={entry_name}",
        ],
    )


if __name__ == "__main__":
    main()
