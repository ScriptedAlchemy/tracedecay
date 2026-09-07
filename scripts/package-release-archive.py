#!/usr/bin/env python3
"""Build a byte-reproducible release archive."""

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
    parser.add_argument(
        "--companion",
        action="append",
        default=[],
        metavar="PATH=ENTRY_NAME",
        help="include a non-executable runtime companion beside the binary",
    )
    parser.add_argument("--epoch", type=int, required=True)
    return parser.parse_args()


def validate_entry_name(entry_name: str) -> None:
    if not entry_name or Path(entry_name).name != entry_name:
        raise ValueError("entry name must be a nonempty basename")


def write_tar_gz(
    output: Path, entries: list[tuple[str, bytes, int]], epoch: int
) -> None:
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
                for entry_name, payload, mode in entries:
                    entry = tarfile.TarInfo(entry_name)
                    entry.size = len(payload)
                    entry.mode = mode
                    entry.uid = 0
                    entry.gid = 0
                    entry.uname = ""
                    entry.gname = ""
                    entry.mtime = epoch
                    archive.addfile(entry, io.BytesIO(payload))


def write_zip(
    output: Path, entries: list[tuple[str, bytes, int]], epoch: int
) -> None:
    timestamp = time.gmtime(epoch)[:6]
    year = timestamp[0]
    if year < 1980 or year > 2107:
        raise ValueError("ZIP epoch must be between 1980 and 2107")
    timestamp = (*timestamp[:5], timestamp[5] - timestamp[5] % 2)

    with zipfile.ZipFile(output, mode="w") as archive:
        for entry_name, payload, mode in entries:
            entry = zipfile.ZipInfo(entry_name, timestamp)
            entry.create_system = 3
            entry.external_attr = (stat.S_IFREG | mode) << 16
            entry.compress_type = zipfile.ZIP_STORED
            entry.extra = b""
            entry.comment = b""
            archive.writestr(entry, payload)


def read_entry(path: Path, entry_name: str, mode: int) -> tuple[str, bytes, int]:
    validate_entry_name(entry_name)
    if not path.is_file():
        raise FileNotFoundError(f"release input does not exist: {path}")
    payload = path.read_bytes()
    if not payload:
        raise ValueError(f"release input is empty: {path}")
    return entry_name, payload, mode


def parse_companion(spec: str) -> tuple[Path, str]:
    path_text, separator, entry_name = spec.rpartition("=")
    if not separator or not path_text or not entry_name:
        raise ValueError("companion must use PATH=ENTRY_NAME")
    return Path(path_text), entry_name


def main() -> None:
    args = parse_args()
    validate_entry_name(args.entry_name)
    if args.epoch < 0:
        raise ValueError("epoch must be nonnegative")
    entries = [read_entry(args.binary, args.entry_name, 0o755)]
    entries.extend(
        read_entry(path, entry_name, 0o644)
        for path, entry_name in map(parse_companion, args.companion)
    )
    entry_names = [entry_name for entry_name, _, _ in entries]
    if len(set(entry_names)) != len(entry_names):
        raise ValueError("release archive entry names must be unique")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.format == "tar.gz":
        write_tar_gz(args.output, entries, args.epoch)
    else:
        write_zip(args.output, entries, args.epoch)


if __name__ == "__main__":
    main()
