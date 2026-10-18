#!/usr/bin/env python3
"""Extend release checksums only when every previously published line survives."""

from __future__ import annotations

import argparse
import subprocess
import tempfile
from pathlib import Path


def requires_update(published: bytes, current: bytes) -> bool:
    if published == current:
        return False
    new_lines = set(current.split(b"\n"))
    for line in published.split(b"\n"):
        if line and line not in new_lines:
            raise ValueError(f"published SHA256SUMS line is not in the new manifest: {line.decode(errors='replace')}")
    return True


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--checksums", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        subprocess.run(
            ["gh", "release", "download", args.tag, "--repo", args.repo,
             "--pattern", "SHA256SUMS", "--dir", directory], check=True,
        )
        published = (Path(directory) / "SHA256SUMS").read_bytes()
        if requires_update(published, args.checksums.read_bytes()):
            subprocess.run(
                ["gh", "release", "upload", args.tag, str(args.checksums),
                 "--clobber", "--repo", args.repo], check=True,
            )


if __name__ == "__main__":
    main()
