#!/usr/bin/env python3
"""Declares NOT NULL on primary-key columns named by the schema portability lint.

Reads `table.column` lines (or full lint lines such as
`project store: metadata.key: nullable PRIMARY KEY column`) on stdin and adds
`NOT NULL` after the column's declared type in every `CREATE TABLE` of that
table in non-test Rust sources under crates/. tracedecay-query is skipped.
Its lexical artifact is outside this rewrite.

Usage: python3 scripts/codemod-pk-not-null.py < violations.txt
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def targets(lines):
    found = set()
    for line in lines:
        match = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\.([A-Za-z_][A-Za-z0-9_]*)(?::|\s*$)", line)
        if match:
            found.add((match.group(1), match.group(2)))
    return found


def source_files():
    for path in sorted((ROOT / "crates").rglob("*.rs")):
        relative = path.relative_to(ROOT).as_posix()
        if (
            relative.startswith("crates/tracedecay-query/")
            or "/tests/" in relative
            or path.name.endswith("tests.rs")
            or path.name == "test_support.rs"
        ):
            continue
        yield path


def table_bodies(text, table):
    head = re.compile(
        r"CREATE TABLE(?: IF NOT EXISTS)? \"?" + re.escape(table) + r"\"?\s*\("
    )
    for match in head.finditer(text):
        depth = 1
        index = match.end()
        while depth and index < len(text):
            depth += {"(": 1, ")": -1}.get(text[index], 0)
            index += 1
        yield match.end(), index


def patch(text, table, column):
    column_definition = re.compile(
        r"(^\s*|[(,]\s*)(" + re.escape(column) + r"\s+[A-Z]+)([^,\n]*)",
        re.MULTILINE,
    )

    def declare(match):
        if "NOT NULL" in match.group(3):
            return match.group(0)
        return f"{match.group(1)}{match.group(2)} NOT NULL{match.group(3)}"

    definitions = 0
    for start, end in reversed(list(table_bodies(text, table))):
        body, count = column_definition.subn(declare, text[start:end], count=1)
        if count:
            text = text[:start] + body + text[end:]
            definitions += 1
    return text, definitions


CONTRACT = ROOT / "crates/tracedecay-global-db/src/schema_contract/definitions.rs"


def patch_contract():
    """Pins NOT NULL for every non-INTEGER primary-key column of the registered
    global-db contract; a lone INTEGER key is the rowid and reports nullable."""
    text = CONTRACT.read_text()
    patched = re.sub(
        r'column\("(\w+)", "(?!INTEGER")(\w+)", false, ([^,]+), ([1-9][0-9]*)\)',
        r'column("\1", "\2", true, \3, \4)',
        text,
    )
    if patched != text:
        CONTRACT.write_text(patched)
        print(CONTRACT.relative_to(ROOT))


def main():
    patch_contract()
    wanted = targets(sys.stdin)
    hits = {key: 0 for key in wanted}
    for path in source_files():
        original = path.read_text()
        text = original
        for table, column in wanted:
            text, edits = patch(text, table, column)
            hits[(table, column)] += edits
        if text != original:
            path.write_text(text)
            print(path.relative_to(ROOT))
    missing = sorted(f"{table}.{column}" for (table, column), count in hits.items() if not count)
    if missing:
        sys.exit("no definition patched for: " + ", ".join(missing))


if __name__ == "__main__":
    main()
