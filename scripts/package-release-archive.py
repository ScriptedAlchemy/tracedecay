#!/usr/bin/env python3
"""Build a byte-reproducible single-binary release archive."""

from __future__ import annotations

import argparse
import gzip
import io
import stat
import tarfile
import time
import zipfile
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--format", choices=("tar.gz", "zip"), required=True)
    parser.add_argument("--entry-name", required=True)
    parser.add_argument("--epoch", type=int, required=True)
    return parser.parse_args()


def validate_entry_name(entry_name: str) -> None:
    if not entry_name or Path(entry_name).name != entry_name:
        raise ValueError("entry name must be a nonempty basename")


def write_tar_gz(output: Path, entry_name: str, payload: bytes, epoch: int) -> None:
    with output.open("wb") as raw:
        with gzip.GzipFile(
            filename="",
            mode="wb",
            fileobj=raw,
            compresslevel=9,
            mtime=epoch,
        ) as compressed:
            with tarfile.open(
                fileobj=compressed,
                mode="w",
                format=tarfile.USTAR_FORMAT,
            ) as archive:
                entry = tarfile.TarInfo(entry_name)
                entry.size = len(payload)
                entry.mode = 0o755
                entry.uid = 0
                entry.gid = 0
                entry.uname = ""
                entry.gname = ""
                entry.mtime = epoch
                archive.addfile(entry, io.BytesIO(payload))


def write_zip(output: Path, entry_name: str, payload: bytes, epoch: int) -> None:
    timestamp = time.gmtime(epoch)[:6]
    year = timestamp[0]
    if year < 1980 or year > 2107:
        raise ValueError("ZIP epoch must be between 1980 and 2107")
    timestamp = (*timestamp[:5], timestamp[5] - timestamp[5] % 2)

    entry = zipfile.ZipInfo(entry_name, timestamp)
    entry.create_system = 3
    entry.external_attr = (stat.S_IFREG | 0o755) << 16
    entry.compress_type = zipfile.ZIP_STORED
    entry.extra = b""
    entry.comment = b""
    with zipfile.ZipFile(output, mode="w") as archive:
        archive.writestr(entry, payload)


def main() -> None:
    args = parse_args()
    validate_entry_name(args.entry_name)
    if args.epoch < 0:
        raise ValueError("epoch must be nonnegative")
    if not args.binary.is_file():
        raise FileNotFoundError(f"binary does not exist: {args.binary}")
    payload = args.binary.read_bytes()
    if not payload:
        raise ValueError(f"binary is empty: {args.binary}")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.format == "tar.gz":
        write_tar_gz(args.output, args.entry_name, payload, args.epoch)
    else:
        write_zip(args.output, args.entry_name, payload, args.epoch)


if __name__ == "__main__":
    main()
