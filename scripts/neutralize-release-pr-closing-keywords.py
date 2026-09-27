#!/usr/bin/env python3
"""Replace GitHub closing keywords in a release PR body.

Release-please copies commit bodies into the release PR. Merging that PR
then closes every issue those keywords name, including issues that were
only referenced. The release-please config schema has header, footer, and
changelog-section keys, and no key that rewrites closing keywords. `Refs`
is not a closing keyword, so the issue number stays readable.

Reads the body on stdin and writes the rewritten body on stdout.
"""

from __future__ import annotations

import re
import sys

# Case-insensitive. GitHub still closes an issue when the keyword is
# lowercased or wrapped in backticks, so the keyword itself has to change.
# Release-please writes the reference as a markdown link, `closes [#N](url)`.
_CLOSING_KEYWORD = re.compile(
    r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\b"
    r"(\s*:?\s*\[?)"
    r"(#\d+|GH-\d+|[\w.-]+/[\w.-]+#\d+|https://github\.com/[\w.-]+/[\w.-]+/issues/\d+)"
)


def neutralize_closing_keywords(body: str) -> str:
    return _CLOSING_KEYWORD.sub(r"Refs\1\2", body)


def main() -> int:
    sys.stdout.write(neutralize_closing_keywords(sys.stdin.read()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
