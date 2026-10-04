#!/usr/bin/env python3
"""Enforce the rust-cache workspace roots repo-wide.

Every rust-cache step that builds this checkout must root its workspace at
``crates`` (see ``assert_vendored_workspace_roots``).

Stdlib only: the pull-request cache-hygiene job checkouts scripts and
workflows onto a bare runner.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parents[1] / ".github" / "workflows"
VENDORED_ROOT = "crates -> ../target"
LOCKFILE_KEY_ROOT = ". -> target/rust-cache-lockfile-key"

RUST_CACHE_USES = re.compile(r"^[ \t]+(?:- )?uses: Swatinem/rust-cache@", re.MULTILINE)


def fail(message: str) -> None:
    print(f"rust-cache lineage policy violation: {message}", file=sys.stderr)
    raise SystemExit(1)


def rust_cache_steps(text: str) -> list[str]:
    """Return the text of each rust-cache step (from its `- ` line to the next sibling)."""
    lines = text.splitlines()
    steps: list[str] = []
    for match in RUST_CACHE_USES.finditer(text):
        index = text.count("\n", 0, match.start())
        while not lines[index].lstrip().startswith("- "):
            index -= 1
        indent = len(lines[index]) - len(lines[index].lstrip())
        end = index + 1
        while end < len(lines) and (
            not lines[end].strip() or len(lines[end]) - len(lines[end].lstrip()) > indent
        ):
            end += 1
        steps.append("\n".join(lines[index:end]))
    return steps


def assert_vendored_workspace_roots() -> None:
    """Every rust-cache step that builds this checkout must root at crates/.

    `.cargo/config.toml` vendors crates-io and git sources under `.pnpm/crates`
    inside the repo. rust-cache keeps only packages outside the workspace
    root, so the default `.` root deletes every dependency artifact and saves
    empty entries. The second root contributes Cargo.lock to the key only.
    """
    for path in sorted(WORKFLOWS.glob("*.yml")):
        text = path.read_text(encoding="utf-8")
        if "actions/checkout@" not in text:
            continue
        for step in rust_cache_steps(text):
            for root in (VENDORED_ROOT, LOCKFILE_KEY_ROOT):
                if root not in step:
                    fail(
                        f"{path.name} rust-cache step must list workspace root {root!r} "
                        "so vendored .pnpm/crates dependencies are cached"
                    )


def main() -> int:
    assert_vendored_workspace_roots()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
