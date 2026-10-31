#!/usr/bin/env python3
"""Decide whether a pull request's changed paths can reach a Bazel action.

Bazel reruns only the actions whose inputs changed, but every Bazel CI job
still pays a hosted runner, Bazel setup, and analysis, and a cold cache turns
that into a full build. Paths that no Bazel target and no Bazel-backed CI
step reads cannot change any of that, so the Bazel jobs skip pull requests
confined to them. The rule is an explicit allowlist: anything else, an empty
list, or a list that does not match the pull request's own file count keeps
the Bazel jobs running.
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

# `mockups/` is in `.bazelignore` and read by nothing in CI. `docs/` has no
# BUILD file, no Bazel label, and no `include_str!` reader. Repository-root
# Markdown is read by no target either; Markdown under `plugin/**` is a Bazel
# input (the plugin bundles glob it), so only the root qualifies.
SKIPPABLE_DIRECTORIES = frozenset({"mockups", "docs"})


def path_is_skippable(path: str) -> bool:
    """Return whether one repository-relative path is outside every Bazel input."""
    if not path or "\\" in path:
        return False
    # Split literally: a normalizing path type would hide `docs//x` or `./`.
    parts = path.split("/")
    if any(part in {"", ".", ".."} for part in parts):
        return False
    if len(parts) > 1:
        return parts[0] in SKIPPABLE_DIRECTORIES
    return path.endswith(".md")


def changed_paths_reach_bazel(
    changes: list[tuple[str, str]], expected_count: int | None
) -> bool:
    """Return whether the Bazel jobs must run for these (path, previous path) pairs.

    `expected_count` is the pull request's own changed-file count; a listing
    that disagrees with it is truncated or stale, which keeps the jobs running.
    """
    if not changes:
        return True
    current = {path for path, _ in changes if path}
    if len(current) != len(changes):
        return True
    if expected_count is not None and expected_count != len(current):
        return True
    for path, previous in changes:
        if not path_is_skippable(path):
            return True
        if previous and not path_is_skippable(previous):
            return True
    return False


def read_changes(listing: Path) -> list[tuple[str, str]]:
    changes: list[tuple[str, str]] = []
    for line in listing.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        path, _, previous = line.partition("\t")
        changes.append((path, previous))
    return changes


def decide(event_name: str, listing: Path | None, expected_count: int | None) -> bool:
    if event_name != "pull_request":
        return True
    if listing is None:
        return True
    return changed_paths_reach_bazel(read_changes(listing), expected_count)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--event-name", required=True)
    parser.add_argument(
        "--changed-paths",
        type=Path,
        help="File with one `path<TAB>previous_path` line per changed file.",
    )
    parser.add_argument(
        "--expected-count",
        default="",
        help="The pull request's changed_files count; empty when unknown.",
    )
    args = parser.parse_args(argv)
    expected = int(args.expected_count) if args.expected_count.strip() else None
    bazel = decide(args.event_name, args.changed_paths, expected)
    line = f"bazel={'true' if bazel else 'false'}"
    print(line)
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
