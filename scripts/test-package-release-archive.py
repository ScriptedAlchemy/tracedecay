#!/usr/bin/env python3
"""Behavioral tests for deterministic release archives."""

from __future__ import annotations

import os
import stat
import subprocess
import tarfile
import tempfile
import time
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
PACKAGER = ROOT / "scripts" / "package-release-archive.py"
EPOCH = 1_700_000_001
PAYLOAD = b"tracedecay release binary\n"


def package(
    binary: Path, output: Path, archive_format: str, entry_name: str
) -> None:
    subprocess.run(
        [
            "python3",
            str(PACKAGER),
            "--binary",
            str(binary),
            "--output",
            str(output),
            "--format",
            archive_format,
            "--entry-name",
            entry_name,
            "--epoch",
            str(EPOCH),
        ],
        check=True,
    )


def test_tar_gz(temp: Path, binary: Path) -> None:
    first = temp / "first.tar.gz"
    second = temp / "second.tar.gz"
    package(binary, first, "tar.gz", "tracedecay")
    os.utime(binary, (EPOCH + 100, EPOCH + 100))
    package(binary, second, "tar.gz", "tracedecay")
    assert first.read_bytes() == second.read_bytes()

    with tarfile.open(first, "r:gz") as archive:
        entries = archive.getmembers()
        assert len(entries) == 1
        entry = entries[0]
        assert entry.name == "tracedecay"
        assert entry.mode == 0o755
        assert entry.uid == 0 and entry.gid == 0
        assert entry.uname == "" and entry.gname == ""
        assert entry.mtime == EPOCH
        extracted = archive.extractfile(entry)
        assert extracted is not None and extracted.read() == PAYLOAD


def test_zip(temp: Path, binary: Path) -> None:
    first = temp / "first.zip"
    second = temp / "second.zip"
    package(binary, first, "zip", "tracedecay.exe")
    os.utime(binary, (EPOCH + 200, EPOCH + 200))
    package(binary, second, "zip", "tracedecay.exe")
    assert first.read_bytes() == second.read_bytes()

    with zipfile.ZipFile(first) as archive:
        entries = archive.infolist()
        assert len(entries) == 1
        entry = entries[0]
        expected_time = list(time.gmtime(EPOCH)[:6])
        expected_time[5] -= expected_time[5] % 2
        assert entry.filename == "tracedecay.exe"
        assert entry.date_time == tuple(expected_time)
        assert entry.compress_type == zipfile.ZIP_STORED
        assert stat.S_IMODE(entry.external_attr >> 16) == 0o755
        assert archive.read(entry) == PAYLOAD


def main() -> None:
    with tempfile.TemporaryDirectory() as temp_name:
        temp = Path(temp_name)
        binary = temp / "binary"
        binary.write_bytes(PAYLOAD)
        binary.chmod(0o755)
        test_tar_gz(temp, binary)
        test_zip(temp, binary)
    print("release archive packaging tests passed")


if __name__ == "__main__":
    main()
