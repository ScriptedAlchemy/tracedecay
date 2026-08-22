#!/usr/bin/env python3
"""Resolve the newest regular ONNX Runtime library in ort's binary cache."""

from __future__ import annotations

import argparse
import os
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--cache-root",
        type=Path,
        default=Path(
            os.environ.get("ORT_CACHE_DIR", Path.home() / ".cache" / "ort.pyke.io")
        ),
    )
    return parser.parse_args()


def is_runtime_library(path: Path) -> bool:
    return (
        path.name in {"libonnxruntime.a", "libonnxruntime.dylib", "onnxruntime.lib"}
        or path.name.startswith("libonnxruntime.so")
    )


def main() -> None:
    args = parse_args()
    candidates = [
        path
        for path in args.cache_root.glob("dfbin/**/*")
        if path.is_file() and not path.is_symlink() and is_runtime_library(path)
    ]
    if candidates:
        newest = max(candidates, key=lambda path: (path.stat().st_mtime_ns, str(path)))
        print(newest.parent.resolve())


if __name__ == "__main__":
    main()
