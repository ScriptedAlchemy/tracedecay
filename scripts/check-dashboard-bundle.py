#!/usr/bin/env python3
"""Reject missing, incomplete, or placeholder dashboard production bundles."""

from __future__ import annotations

import argparse
import hashlib
import json
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


# Version prefix of the skip-mode bundle digest contract shared with the
# tracedecay-cli build script: TRACEDECAY_SKIP_DASHBOARD_BUILD requires
# TRACEDECAY_DASHBOARD_BUNDLE_SHA256 to carry this digest of the prebuilt
# bundle. Both implementations must hash exactly the same byte stream.
BUNDLE_DIGEST_PREFIX = b"tracedecay-dashboard-bundle-v1\0"


def manifest_relative_paths(bundle: Path) -> list[PurePosixPath]:
    manifest_path = bundle / "asset-manifest.json"
    if not manifest_path.is_file():
        raise ValueError(
            f"dashboard bundle: asset-manifest.json is missing: {manifest_path}"
        )
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise ValueError(
            f"dashboard bundle: asset-manifest.json is invalid JSON: {error}"
        ) from error
    all_files = manifest.get("allFiles")
    if not isinstance(all_files, list) or not all(
        isinstance(entry, str) for entry in all_files
    ):
        raise ValueError(
            "dashboard bundle: asset-manifest.json allFiles must be a string array"
        )

    normalized: set[PurePosixPath] = set()
    for entry in all_files:
        relative = local_asset_path(entry)
        if relative is None:
            raise ValueError(
                f"dashboard bundle: allFiles entry is not a local path: {entry}"
            )
        normalized.add(relative)
    return sorted(normalized)


def bundle_digest(bundle: Path) -> str:
    digest = hashlib.sha256()
    digest.update(BUNDLE_DIGEST_PREFIX)
    for relative in manifest_relative_paths(bundle):
        asset = bundle.joinpath(*relative.parts)
        if not asset.is_file():
            raise ValueError(
                f"dashboard bundle: manifest-listed file is missing: {asset}"
            )
        contents = asset.read_bytes()
        digest.update(str(relative).encode("utf-8"))
        digest.update(b"\x00")
        digest.update(len(contents).to_bytes(8, "little"))
        digest.update(contents)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "bundle",
        nargs="?",
        type=Path,
        default=Path("dashboard/app-dist"),
    )
    parser.add_argument(
        "--print-digest",
        action="store_true",
        help=(
            "after validating, print the skip-mode bundle digest expected in "
            "TRACEDECAY_DASHBOARD_BUNDLE_SHA256"
        ),
    )
    args = parser.parse_args()

    try:
        validate_bundle(args.bundle)
        if args.print_digest:
            print(bundle_digest(args.bundle))
    except (OSError, UnicodeError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
