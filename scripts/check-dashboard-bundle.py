#!/usr/bin/env python3
"""Reject missing, incomplete, or placeholder dashboard production bundles."""

from __future__ import annotations

import argparse
from html.parser import HTMLParser
from pathlib import Path, PurePosixPath
from urllib.parse import unquote, urlsplit


MIN_JAVASCRIPT_BYTES = 704


class AssetReferenceParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.references: list[tuple[str, str]] = []

    def handle_starttag(
        self, tag: str, attrs: list[tuple[str, str | None]]
    ) -> None:
        attributes = dict(attrs)
        if tag == "script" and attributes.get("src"):
            self.references.append((tag, attributes["src"]))
        elif tag == "link" and attributes.get("href"):
            self.references.append((tag, attributes["href"]))


def local_asset_path(raw_reference: str) -> PurePosixPath | None:
    parsed = urlsplit(raw_reference)
    if parsed.scheme or parsed.netloc or not parsed.path:
        return None

    decoded = unquote(parsed.path).lstrip("/")
    relative = PurePosixPath(decoded)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"dashboard bundle: unsafe local asset reference: {raw_reference}")
    return relative


def validate_bundle(bundle: Path) -> None:
    if not bundle.is_dir():
        raise ValueError(f"dashboard bundle: bundle directory is missing: {bundle}")

    index = bundle / "index.html"
    if not index.is_file():
        raise ValueError(f"dashboard bundle: index.html is missing: {index}")
    if index.stat().st_size == 0:
        raise ValueError(f"dashboard bundle: index.html is empty: {index}")

    parser = AssetReferenceParser()
    parser.feed(index.read_text(encoding="utf-8"))

    javascript_sizes: list[int] = []
    for tag, raw_reference in parser.references:
        relative = local_asset_path(raw_reference)
        if relative is None:
            continue

        asset = bundle.joinpath(*relative.parts)
        if not asset.is_file():
            raise ValueError(
                "dashboard bundle: referenced asset is missing: "
                f"{raw_reference} ({asset})"
            )
        size = asset.stat().st_size
        if size == 0:
            raise ValueError(
                "dashboard bundle: referenced asset is empty: "
                f"{raw_reference} ({asset})"
            )
        if tag == "script" and relative.suffix in {".js", ".mjs"}:
            javascript_sizes.append(size)

    if not javascript_sizes:
        raise ValueError(
            "dashboard bundle: index.html does not load a local JavaScript asset"
        )
    if max(javascript_sizes) <= MIN_JAVASCRIPT_BYTES:
        raise ValueError(
            "dashboard bundle: JavaScript payload is placeholder-sized "
            f"(largest referenced file is {max(javascript_sizes)} bytes)"
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "bundle",
        nargs="?",
        type=Path,
        default=Path("dashboard/app-dist"),
    )
    args = parser.parse_args()

    try:
        validate_bundle(args.bundle)
    except (OSError, UnicodeError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
