#!/usr/bin/env python3
"""Strict JSON-object decoding shared by V2 execution tooling."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def loads_object(payload: bytes, label: str) -> dict[str, Any]:
    """Decode one JSON object while rejecting duplicate keys and non-finite values."""

    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"{label}: duplicate JSON key {key!r}")
            result[key] = value
        return result

    value = json.loads(
        payload,
        object_pairs_hook=unique_object,
        parse_constant=lambda item: (_ for _ in ()).throw(
            ValueError(f"{label}: non-finite constant {item!r}")
        ),
    )
    if not isinstance(value, dict):
        raise ValueError(f"{label}: root must be an object")
    return value


def load_object(path: Path, label: str) -> dict[str, Any]:
    return loads_object(path.read_bytes(), label)
