#!/usr/bin/env python3
"""Restore-lineage helpers for Swatinem/rust-cache Actions keys.

Swatinem/rust-cache saves

    v<N>-rust-<shared-or-job>-<os>-<arch>-<env8>-<lock8>

and restores by prefix

    v<N>-rust-<shared-or-job>-<os>-<arch>-<env8>-

rustc writes dependency ``.rmeta`` files as mode 0444. A prefix restore
therefore reinstalls another lockfile generation's read-only metadata, and
rustc fails when it tries to update those files in place. Lineage is the
lane identity without the generation number or the trailing hashes, so a
newer generation (or a newer env/lock pair) can supersede an older entry
of the same hosted lane.
"""

from __future__ import annotations

import re

# Hosted hotpath lanes that previously restored ``v0-rust-`` blobs containing
# rustc-readonly ``.rmeta``. Bumping the prefix stops those restores without
# discarding the cargo registry on an exact later hit.
CACHE_GENERATION = 1
PREFIX_KEY = f"v{CACHE_GENERATION}-rust"

KEY_RE = re.compile(
    r"^v(?P<generation>\d+)-rust-(?P<lineage>.+)-[0-9a-f]{8}-[0-9a-f]{8}$"
)


def lineage_of(key: str) -> str | None:
    """Return the restore lineage, or None when the key is not rust-cache."""
    match = KEY_RE.match(key)
    if match is None:
        return None
    return match.group("lineage")


def generation_of(key: str) -> int | None:
    match = KEY_RE.match(key)
    if match is None:
        return None
    return int(match.group("generation"))
