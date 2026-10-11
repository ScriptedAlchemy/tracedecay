#!/usr/bin/env python3
"""Declare NOT NULL on every primary-key column of a STRICT table.

SQLite enforces NOT NULL on the primary key of a STRICT table implicitly; the
native engine does not, so the constraint must be spelled out. A lone INTEGER
PRIMARY KEY is the rowid and is left alone. DDL inside `#[cfg(test)]` modules is
skipped, so pinned released shapes keep their historical text.

Usage: strict-key-not-null.py [--check] FILE...
  --check  list offending columns and exit 1 if any; edit nothing.
"""

import re
import sys

CREATE = re.compile(
    r"CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([\w\"`\[\].]+)\s*\(", re.IGNORECASE
)
TABLE_CONSTRAINT = {"CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"}


def skip_string(text, i):
    """Return the index after the single-quoted SQL string starting at i."""
    i += 1
    while i < len(text):
        if text[i] == "'":
            if text[i + 1 : i + 2] == "'":
                i += 2
                continue
            return i + 1
        i += 1
    raise ValueError("unterminated string")


def skip_comment(text, i):
    end = text.find("\n", i)
    return len(text) if end < 0 else end


def skip_group(text, i):
    """Return the index after the parenthesized group opening at i."""
    depth = 0
    while i < len(text):
        c = text[i]
        if c == "'":
            i = skip_string(text, i)
            continue
        if text.startswith("--", i):
            i = skip_comment(text, i)
            continue
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    raise ValueError("unbalanced parentheses")


def elements(text, start, end):
    """Split text[start:end] at top-level commas into lists of (kind, value, start, end) tokens."""
    result, current, i = [], [], start
    while i < end:
        c = text[i]
        if c.isspace() or c == "\\":
            i += 1
        elif text.startswith("--", i):
            i = skip_comment(text, i)
        elif c == ",":
            result.append(current)
            current = []
            i += 1
        elif c == "'":
            j = skip_string(text, i)
            current.append(("string", text[i:j], i, j))
            i = j
        elif c == "(":
            j = skip_group(text, i)
            current.append(("group", text[i + 1 : j - 1], i, j))
            i = j
        else:
            j = i
            while j < end and not (text[j].isspace() or text[j] in ",()'\\"):
                j += 1
            current.append(("word", text[i:j], i, j))
            i = j
    result.append(current)
    return [element for element in result if element]


def has_sequence(words, first, second):
    return any(a == first and b == second for a, b in zip(words, words[1:]))


def unquote(name):
    return name.strip("\"`[]").lower()


def key_offenders(text, body_start, body_end):
    """Return (column, insert_offset) for each nullable primary-key column."""
    columns, table_key = {}, []
    for element in elements(text, body_start, body_end):
        words = [value.upper() if kind == "word" else None for kind, value, _, _ in element]
        if words[0] in TABLE_CONSTRAINT:
            for index, word in enumerate(words):
                if word == "PRIMARY" and index + 2 < len(element):
                    group = element[index + 2]
                    table_key = [
                        unquote(part.split()[0]) for part in group[1].split(",") if part.strip()
                    ]
            continue
        name = unquote(element[0][1])
        type_token = element[1] if len(element) > 1 and element[1][0] == "word" else None
        columns[name] = {
            "type": type_token[1].upper() if type_token else None,
            "insert_at": type_token[3] if type_token else element[0][3],
            "primary_key": has_sequence(words, "PRIMARY", "KEY"),
            "not_null": has_sequence(words, "NOT", "NULL"),
        }
    key = table_key or [name for name, column in columns.items() if column["primary_key"]]
    if len(key) == 1 and columns[key[0]]["type"] == "INTEGER":
        return []
    return [(name, columns[name]["insert_at"]) for name in key if not columns[name]["not_null"]]


def test_module_spans(text):
    spans = []
    for match in re.finditer(r"^#\[cfg\(test\)\]\n(?:#\[.*\]\n)*(?:pub(?:\(\w+\))? )?mod \w+ \{\n", text, re.M):
        close = re.compile(r"^\}$", re.M).search(text, match.end())
        spans.append((match.start(), close.end() if close else len(text)))
    return spans


def scan(path, text):
    skipped = test_module_spans(text) if path.endswith(".rs") else []
    found = []
    for match in CREATE.finditer(text):
        if any(start <= match.start() < end for start, end in skipped):
            continue
        open_paren = match.end() - 1
        try:
            close = skip_group(text, open_paren)
        except ValueError:
            continue
        if not re.match(r"\s*STRICT\b", text[close:], re.IGNORECASE):
            continue
        table = unquote(match.group(1))
        for column, offset in key_offenders(text, open_paren + 1, close - 1):
            found.append((table, column, offset))
    return found


def main(argv):
    check = "--check" in argv
    paths = [arg for arg in argv if arg != "--check"]
    if not paths:
        print(__doc__, file=sys.stderr)
        return 2
    total = 0
    for path in paths:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        found = scan(path, text)
        for table, column, offset in found:
            line = text.count("\n", 0, offset) + 1
            print(f"{path}:{line}: {table}.{column}: nullable STRICT primary-key column")
        total += len(found)
        if found and not check:
            for _, _, offset in sorted(found, key=lambda item: item[2], reverse=True):
                text = text[:offset] + " NOT NULL" + text[offset:]
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(text)
    return 1 if check and total else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
