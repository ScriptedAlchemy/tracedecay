#!/usr/bin/env python3
"""Reject missing, incomplete, or placeholder dashboard production bundles."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
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
    if (bundle / "asset-manifest.json").exists():
        read_dashboard_manifest(bundle)


# Version prefix of the skip-mode bundle digest contract shared with the
# tracedecay-cli build script: TRACEDECAY_SKIP_DASHBOARD_BUILD requires
# TRACEDECAY_DASHBOARD_BUNDLE_SHA256 to carry this digest of the prebuilt
# bundle. Both implementations must hash exactly the same byte stream.
BUNDLE_DIGEST_PREFIX = b"tracedecay-dashboard-bundle-v1\0"
# Only encoded bundles append this section, so identity-only digests keep
# their shipped byte contract. Pair entries use the file entry encoding with
# the normalized payload path as contents, ordered by normalized source path.
GZIP_MAPPING_DIGEST_PREFIX = b"\0tracedecay-dashboard-gzip-v1\0"


def normalize_manifest_path(entry: str) -> PurePosixPath:
    relative = PurePosixPath(entry)
    if (
        not entry
        or any(character in entry for character in "\\?#%:")
        or relative.is_absolute()
        or ".." in relative.parts
        or not relative.parts
    ):
        raise ValueError(f"dashboard bundle: unsafe manifest path: {entry}")
    return relative


def read_dashboard_manifest(
    bundle: Path,
) -> tuple[list[PurePosixPath], dict[PurePosixPath, PurePosixPath]]:
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
    if not isinstance(manifest, dict):
        raise ValueError("dashboard bundle: asset-manifest.json must be an object")
    all_files = manifest.get("allFiles")
    if not isinstance(all_files, list) or not all(
        isinstance(entry, str) for entry in all_files
    ):
        raise ValueError(
            "dashboard bundle: asset-manifest.json allFiles must be a string array"
        )

    normalized: set[PurePosixPath] = set()
    for entry in all_files:
        relative = normalize_manifest_path(entry)
        if relative in normalized:
            raise ValueError(f"dashboard bundle: duplicate manifest path: {entry}")
        normalized.add(relative)
    if PurePosixPath("index.html") not in normalized:
        raise ValueError("dashboard bundle: allFiles must contain index.html")

    mapping = manifest.get("gzip", {})
    if not isinstance(mapping, dict):
        raise ValueError("dashboard bundle: manifest gzip must be an object")
    compressed_paths: dict[PurePosixPath, PurePosixPath] = {}
    for source, encoded in mapping.items():
        if not isinstance(encoded, str):
            raise ValueError("dashboard bundle: gzip payload path must be a string")
        source_path = normalize_manifest_path(source)
        encoded_path = normalize_manifest_path(encoded)
        if (
            encoded_path != PurePosixPath(f"{source_path}.gz")
            or source_path not in normalized
            or encoded_path not in normalized
            or source_path in compressed_paths
        ):
            raise ValueError(f"dashboard bundle: invalid gzip mapping for {source}")
        compressed_paths[source_path] = encoded_path
    for source, encoded in compressed_paths.items():
        if encoded in compressed_paths:
            raise ValueError(f"dashboard bundle: gzip payload is another source: {encoded}")
        raw = bundle.joinpath(*source.parts).read_bytes()
        compressed = bundle.joinpath(*encoded.parts).read_bytes()
        with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as decoder:
            roundtrip = decoder.read(len(raw) + 1)
        if len(compressed) >= len(raw) or roundtrip != raw:
            raise ValueError(f"dashboard bundle: gzip payload does not match {source}")
    return sorted(normalized, key=str), compressed_paths


def bundle_digest(bundle: Path) -> str:
    digest = hashlib.sha256()
    digest.update(BUNDLE_DIGEST_PREFIX)
    asset_paths, compressed_paths = read_dashboard_manifest(bundle)
    for relative in asset_paths:
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
    if compressed_paths:
        digest.update(GZIP_MAPPING_DIGEST_PREFIX)
        digest.update(len(compressed_paths).to_bytes(8, "little"))
        for source in sorted(compressed_paths, key=str):
            encoded = str(compressed_paths[source]).encode("utf-8")
            digest.update(str(source).encode("utf-8"))
            digest.update(b"\x00")
            digest.update(len(encoded).to_bytes(8, "little"))
            digest.update(encoded)
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
    except (EOFError, OSError, UnicodeError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
