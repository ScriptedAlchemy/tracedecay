#!/usr/bin/env python3
"""Validate the real, offline FastEmbed distribution fixture."""

from __future__ import annotations

import hashlib
import json
import pathlib
import re
import sys


SCHEMA = "tracedecay.distribution.fastembed-fixture.v1"
MODEL = "JinaEmbeddingsV2BaseCode"
MEMBERS = {
    "model": "model.onnx",
    "tokenizer": "tokenizer.json",
    "config": "config.json",
    "special_tokens_map": "special_tokens_map.json",
    "tokenizer_config": "tokenizer_config.json",
}
SHA256 = re.compile(r"[0-9a-f]{64}")


def fail(message: str) -> None:
    raise SystemExit(f"FastEmbed distribution fixture: {message}")


def require_positive_integer(document: dict[str, object], key: str, maximum: int) -> int:
    value = document.get(key)
    if not isinstance(value, int) or isinstance(value, bool) or not 1 <= value <= maximum:
        fail(f"{key} must be an integer in 1..={maximum}")
    return value


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: validate_fixture.py FIXTURE_DIRECTORY")

    root = pathlib.Path(sys.argv[1])
    manifest_path = root / "fixture.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        fail(f"missing regular file {manifest_path}")

    try:
        document = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"cannot read {manifest_path}: {error}")
    if not isinstance(document, dict):
        fail("fixture.json must contain an object")
    if document.get("schema") != SCHEMA:
        fail(f"fixture.json schema must be {SCHEMA!r}")
    if document.get("model") != MODEL:
        fail(f"fixture.json model must be {MODEL!r}")

    source = document.get("source")
    if not isinstance(source, dict):
        fail("fixture.json source must be an object")
    for key in ("upstream", "revision", "license", "license_url", "provenance"):
        value = source.get(key)
        if not isinstance(value, str) or not value.strip():
            fail(f"fixture.json source.{key} must be a non-empty string")

    dimensions = require_positive_integer(document, "expected_dimensions", 65_536)
    max_length = require_positive_integer(document, "max_length", 8_192)

    members = document.get("members")
    if not isinstance(members, dict) or set(members) != set(MEMBERS):
        fail("fixture.json members must contain exactly: " + ", ".join(MEMBERS))

    for role, expected_name in MEMBERS.items():
        member = members[role]
        if not isinstance(member, dict):
            fail(f"members.{role} must be an object")
        if member.get("path") != expected_name:
            fail(f"members.{role}.path must be {expected_name!r}")
        expected_length = member.get("length")
        if (
            not isinstance(expected_length, int)
            or isinstance(expected_length, bool)
            or expected_length <= 0
        ):
            fail(f"members.{role}.length must be a positive integer")
        expected_digest = member.get("sha256")
        if not isinstance(expected_digest, str) or SHA256.fullmatch(expected_digest) is None:
            fail(f"members.{role}.sha256 must be 64 lowercase hexadecimal characters")

        path = root / expected_name
        if path.is_symlink() or not path.is_file():
            fail(f"missing regular file {path}")
        try:
            data = path.read_bytes()
        except OSError as error:
            fail(f"cannot read {path}: {error}")
        if not data:
            fail(f"{path} must not be empty")
        if len(data) != expected_length:
            fail(
                f"{path} length mismatch: expected {expected_length}, got {len(data)}"
            )
        actual_digest = hashlib.sha256(data).hexdigest()
        if actual_digest != expected_digest:
            fail(
                f"{path} digest mismatch: expected {expected_digest}, got {actual_digest}"
            )

    print(f"{dimensions}\t{max_length}")


if __name__ == "__main__":
    main()
