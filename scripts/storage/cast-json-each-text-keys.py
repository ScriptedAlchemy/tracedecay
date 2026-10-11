#!/usr/bin/env python3
"""Spell every TEXT key read from json_each as CAST(value AS TEXT) (#3402).

Turso declares json_each.value with NUMERIC affinity, so a TEXT index cannot
seek on it (benchmark_data/storage-concurrency/turso-bugs/
json-each-value-cannot-seek-text-index.md). The cast is a no-op on SQLite.

Rewrites `<column> IN (SELECT value FROM json_each(` when <column> is not an
INTEGER rowid key, in the crates passed on the command line. Prints each
rewritten file; `--check` exits 1 instead of writing when a rewrite is due.
"""

import pathlib
import re
import sys

# Rowid keys seek on NUMERIC affinity already.
INTEGER_KEYS = {"store_id"}
PATTERN = re.compile(r"([A-Za-z_][\w.]*)(\s+IN\s+\(SELECT\s+)value(\s+FROM\s+json_each\()")


def rewrite(text: str) -> str:
    def replace(match: re.Match) -> str:
        column = match.group(1).rsplit(".", 1)[-1]
        if column in INTEGER_KEYS:
            return match.group(0)
        return f"{match.group(1)}{match.group(2)}CAST(value AS TEXT){match.group(3)}"

    return PATTERN.sub(replace, text)


def main() -> int:
    args = sys.argv[1:]
    check = "--check" in args
    roots = [pathlib.Path(arg) for arg in args if arg != "--check"]
    pending = False
    for root in roots:
        for path in sorted(root.rglob("*.rs")):
            text = path.read_text()
            updated = rewrite(text)
            if updated != text:
                pending = True
                print(path)
                if not check:
                    path.write_text(updated)
    return 1 if check and pending else 0


if __name__ == "__main__":
    sys.exit(main())
