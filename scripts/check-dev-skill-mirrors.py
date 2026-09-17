#!/usr/bin/env python3
"""Keep repository dev-skill host copies identical without a hand reconcile.

Shared files under `.claude/skills`, `.codex/skills`, and `.agents/skills`
(when that directory exists) must be the same bytes. Host-private files are
only `agents/openai.yaml` and `*.test.sh`. Any other file that exists in one
tree and not the others is drift.

    scripts/check-dev-skill-mirrors.py check
    scripts/check-dev-skill-mirrors.py sync --from claude
    scripts/check-dev-skill-mirrors.py sync --from codex

`sync` copies shared files from the named tree onto the other present trees
and deletes shared files the source no longer has. It never writes
host-private files.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

HOSTS = ("claude", "codex")
OPTIONAL_ROOTS = (".agents/skills",)


class SkillMirrorError(Exception):
    """The host copies do not match, or the requested tree is missing."""


def is_host_private(relative: Path) -> bool:
    parts = relative.parts
    if len(parts) >= 2 and parts[-2:] == ("agents", "openai.yaml"):
        return True
    return relative.name.endswith(".test.sh")


def iter_files(root: Path) -> list[Path]:
    if not root.is_dir():
        return []
    return sorted(path for path in root.rglob("*") if path.is_file())


def shared_files(root: Path) -> dict[Path, bytes]:
    files: dict[Path, bytes] = {}
    for path in iter_files(root):
        relative = path.relative_to(root)
        if is_host_private(relative):
            continue
        files[relative] = path.read_bytes()
    return files


def present_roots(repo: Path) -> list[tuple[str, Path]]:
    roots = [(name, repo / f".{name}/skills") for name in HOSTS]
    roots.extend((path, repo / path) for path in OPTIONAL_ROOTS if (repo / path).is_dir())
    return roots


def require_host_roots(repo: Path) -> list[tuple[str, Path]]:
    missing = [name for name, path in present_roots(repo)[: len(HOSTS)] if not path.is_dir()]
    if missing:
        joined = ", ".join(f".{name}/skills" for name in missing)
        raise SkillMirrorError(f"missing dev skill tree: {joined}")
    return present_roots(repo)


def check(repo: Path) -> list[str]:
    """Return drift messages. Empty means the shared trees match."""
    roots = require_host_roots(repo)
    by_root = {name: shared_files(path) for name, path in roots}
    names = list(by_root)
    canonical_name = names[0]
    errors: list[str] = []
    for name in names[1:]:
        left = by_root[canonical_name]
        right = by_root[name]
        for relative in sorted(set(left) | set(right)):
            if relative not in left:
                errors.append(f"{name} has shared file missing from {canonical_name}: {relative}")
            elif relative not in right:
                errors.append(f"{canonical_name} has shared file missing from {name}: {relative}")
            elif left[relative] != right[relative]:
                errors.append(f"shared skill bytes differ: {relative} ({canonical_name} vs {name})")
    return errors


def sync(repo: Path, source_name: str) -> list[str]:
    """Copy shared files from `source_name` onto every other present tree."""
    roots = dict(require_host_roots(repo))
    if source_name not in roots:
        known = ", ".join(sorted(roots))
        raise SkillMirrorError(f"unknown --from {source_name}; present trees: {known}")
    source = roots[source_name]
    source_files = shared_files(source)
    actions: list[str] = []
    for name, destination in roots.items():
        if name == source_name:
            continue
        destination.mkdir(parents=True, exist_ok=True)
        current = shared_files(destination)
        for relative, data in sorted(source_files.items()):
            target = destination / relative
            if current.get(relative) == data:
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            actions.append(f"write {name}/{relative.as_posix()}")
        for relative in sorted(set(current) - set(source_files)):
            (destination / relative).unlink()
            actions.append(f"delete {name}/{relative.as_posix()}")
    return actions


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    subcommands = parser.add_subparsers(dest="command", required=True)
    subcommands.add_parser("check")
    sync_parser = subcommands.add_parser("sync")
    sync_parser.add_argument("--from", dest="source", required=True, choices=(*HOSTS, *OPTIONAL_ROOTS))
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    repo = args.root.resolve()
    try:
        if args.command == "check":
            errors = check(repo)
            if errors:
                print("\n".join(errors), file=sys.stderr)
                return 1
            print("dev skill host copies match")
            return 0
        actions = sync(repo, args.source)
        errors = check(repo)
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        if actions:
            print("\n".join(actions))
        else:
            print("dev skill host copies already match")
        return 0
    except SkillMirrorError as error:
        print(str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
