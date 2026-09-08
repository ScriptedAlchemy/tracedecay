#!/usr/bin/env python3
"""Select the Actions cache entries a newer entry of the same lineage supersedes.

Reads `gh api --paginate repos/<owner>/<repo>/actions/caches` output (any
concatenation of list pages, entry arrays or single entries) on stdin and
prints one cache id per line for every entry that can no longer be restored
because a newer entry with the same restore lineage exists on the same ref.

A lineage is the part of a key the workflow restores by prefix:

* `.github/actions/kache` saves `kache-store-<lane>-<os>-<version>-<run>-<attempt>`
  and restores the newest `kache-store-<lane>-<os>-<version>-` entry, so every
  older run's store on the ref is dead weight the moment a newer one lands.
* Swatinem/rust-cache saves `v0-rust-<prefix>-<env hash>-<lockfile hash>` and
  restores `v0-rust-<prefix>-<env hash>-` by prefix, never across an env
  hash, so both an older lockfile generation and an older toolchain
  generation are unreachable.

Keys outside these two shapes (setup-node's `node-cache-…`, arbitrary
`actions/cache` keys) are left alone: the same prefix can legitimately carry
several live entries there (one per lockfile a different job hashes).
"""

from __future__ import annotations

import json
import re
import sys
from collections import defaultdict
from collections.abc import Iterable, Iterator
from typing import Any

LINEAGES = (
    re.compile(r"^(?P<lineage>kache-store-.+-v\d+(?:\.\d+)*)-\d+-\d+$"),
    re.compile(r"^(?P<lineage>v0-rust-.+)-[0-9a-f]{8}-[0-9a-f]{8}$"),
)


def lineage_of(key: str) -> str | None:
    for pattern in LINEAGES:
        match = pattern.match(key)
        if match is not None:
            return match.group("lineage")
    return None


def superseded(entries: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    """Return every entry an entry of the same (ref, lineage) created later supersedes."""
    groups: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for entry in entries:
        lineage = lineage_of(entry["key"])
        if lineage is None:
            continue
        groups[(entry["ref"], lineage)].append(entry)
    doomed: list[dict[str, Any]] = []
    for group in groups.values():
        group.sort(key=lambda entry: (entry["created_at"], entry["id"]), reverse=True)
        doomed.extend(group[1:])
    return doomed


def parse_entries(text: str) -> Iterator[dict[str, Any]]:
    """Yield cache entries from concatenated JSON pages, arrays or entries."""
    decoder = json.JSONDecoder()
    position = 0
    length = len(text)
    while True:
        while position < length and text[position].isspace():
            position += 1
        if position >= length:
            return
        value, position = decoder.raw_decode(text, position)
        yield from entries_in(value)


def entries_in(value: Any) -> Iterator[dict[str, Any]]:
    if isinstance(value, list):
        for item in value:
            yield from entries_in(item)
    elif isinstance(value, dict) and "actions_caches" in value:
        yield from entries_in(value["actions_caches"])
    elif isinstance(value, dict) and {"id", "key", "ref", "created_at"} <= value.keys():
        yield value
    else:
        raise ValueError(f"not an Actions cache listing: {json.dumps(value)[:120]}")


def main() -> int:
    entries = list(parse_entries(sys.stdin.read()))
    doomed = superseded(entries)
    for entry in doomed:
        print(entry["id"])
        print(
            f"superseded {entry['key']} on {entry['ref']} "
            f"({entry.get('size_in_bytes', 0) / 2**20:.0f} MiB, created {entry['created_at']})",
            file=sys.stderr,
        )
    print(
        f"{len(doomed)} of {len(entries)} cache entries superseded "
        f"({sum(entry.get('size_in_bytes', 0) for entry in doomed) / 2**20:.0f} MiB)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
