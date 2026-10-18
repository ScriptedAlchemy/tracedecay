#!/usr/bin/env python3
"""Carry a Claude profile's endpoint/auth block into a throwaway config.

A Claude profile aimed at a non-Anthropic endpoint authenticates through the
`env` block of `settings.json` (`ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY`,
model aliases), not through `.credentials.json`. The eval runner copies only
that block and the profile `model`; permissions, plugins, hooks, and every
other key stay behind so the throwaway profile cannot inherit ambient config.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

CARRIED_KEYS = ("env", "model")


def carried_profile(profile: dict) -> dict:
    return {key: profile[key] for key in CARRIED_KEYS if key in profile}


def carry(src: Path, dest: Path) -> bool:
    """Write the carried subset of `src` to `dest`. Returns True if written."""
    if not src.is_file():
        return False
    carried = carried_profile(json.loads(src.read_text()))
    if not carried:
        return False
    dest.write_text(json.dumps(carried, indent=2) + "\n")
    dest.chmod(0o400)
    return True


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: carry_claude_profile.py <src settings.json> <dest settings.json>", file=sys.stderr)
        return 2
    carry(Path(argv[1]), Path(argv[2]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
