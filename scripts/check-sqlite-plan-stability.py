#!/usr/bin/env python3
"""Keep SQL statements plannable under SQLite's query planner stability guarantee.

Every policy connection enables `SQLITE_DBCONFIG_ENABLE_QPSG`, so SQLite plans a
statement once, without looking at bound values. Two planner features then stop
working for a bound parameter:

- A partial index `CREATE INDEX ... WHERE col = 'literal'` is used only when the
  query names the literal. `WHERE col = ?` silently scans instead.
- `col LIKE ?` and `col GLOB ?` lose the prefix-range optimization.

This check reads every SQL string literal in `crates/*/src` (test modules
excluded), collects the partial indexes those sources create, and reports a
statement that binds a parameter in a comparison against a column or expression
that a partial-index predicate on one of its tables compares with a literal, or
that binds a bare `LIKE`/`GLOB` pattern. A deliberate exception names its reason in a comment on
the literal's line or one of the three lines above it:

    // qpsg-exempt: <why the bound value cannot or need not use the index>

    scripts/check-sqlite-plan-stability.py [--root <repo>]
"""

from __future__ import annotations

import argparse
import bisect
import re
import sys
from dataclasses import dataclass
from pathlib import Path

EXEMPT = re.compile(r"//\s*qpsg-exempt:\s*\S")
EXEMPT_WINDOW = 3
SQL_HEAD = re.compile(r"^\s*(?:--[^\n]*\n\s*)*(SELECT|WITH|UPDATE|DELETE|INSERT|REPLACE)\b", re.I)
TOKEN = re.compile(
    r"""(?P<ws>\s+)|(?P<lc>--[^\n]*)|(?P<bc>/\*.*?\*/)|(?P<str>'(?:[^']|'')*')"""
    r"""|(?P<qid>"(?:[^"]|"")*"|`[^`]*`|\[[^\]]*\])|(?P<param>\?\d*|[:@$][A-Za-z_]\w*)"""
    r"""|(?P<num>\d+(?:\.\d+)?)|(?P<hole>\{[^{}]*\})|(?P<id>[A-Za-z_]\w*)"""
    r"""|(?P<op><=|>=|==|!=|<>|\|\||.)""",
    re.S,
)
CLAUSE_KEYWORDS = {
    "CROSS", "DEFAULT", "EXCEPT", "GROUP", "HAVING", "INDEXED", "INNER", "INTERSECT", "JOIN",
    "LEFT", "LIMIT", "NATURAL", "NOT", "ON", "ORDER", "RETURNING", "SELECT", "SET", "UNION",
    "USING", "VALUES", "WHERE", "WINDOW",
}
COMPARISON = {"=", "==", "<", ">", "<=", ">=", "!=", "<>", "IS"}
INDEX = re.compile(
    r"CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:IF\s+NOT\s+EXISTS\s+)?(?:\w+\.)?(?P<name>\w+)\s+"
    r"ON\s+(?P<table>\w+)\s*\((?P<columns>(?:[^()]|\([^()]*\))*)\)\s*WHERE\s+(?P<predicate>[^;]*)",
    re.I | re.S,
)
ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
RUST_LEXEME = re.compile(r"""//|/\*|(?<![\w])b?r#*"|(?<![\w])b?"|'""")
BLOCK_COMMENT = re.compile(r"/\*|\*/")
CHAR_LITERAL = re.compile(r"'(?:\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'\n])'")
STRING_BODY = re.compile(r'"((?:\\.|[^"\\])*)"', re.S)
STRING_ESCAPE = re.compile(r"\\(\n\s*|\r\n\s*|u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)", re.S)


@dataclass(frozen=True)
class Literal:
    path: str
    line: int
    text: str
    exempt: bool


@dataclass(frozen=True)
class PartialIndex:
    name: str
    table: str
    terms: frozenset[str]


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    message: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.message}"


def decode_escape(match: re.Match[str]) -> str:
    escape = match.group(1)
    if escape[0] in "\r\n":
        return ""
    if escape[0] == "u":
        return chr(int(escape[2:-1], 16))
    if escape[0] == "x":
        return chr(int(escape[1:], 16))
    return ESCAPES.get(escape, escape)


def rust_string_literals(source: str):
    """Yield (offset, decoded text) for each string literal outside comments."""
    i = 0
    while match := RUST_LEXEME.search(source, i):
        start, lexeme = match.start(), match.group()
        if lexeme == "//":
            end = source.find("\n", start)
            i = len(source) if end < 0 else end
        elif lexeme == "/*":
            depth, i = 1, start + 2
            while depth and (close := BLOCK_COMMENT.search(source, i)):
                depth += 1 if close.group() == "/*" else -1
                i = close.end()
            if depth:
                i = len(source)
        elif lexeme == "'":
            char = CHAR_LITERAL.match(source, start)
            i = char.end() if char else start + 1
        elif lexeme.endswith('"') and "r" in lexeme:
            hashes = lexeme[lexeme.index("r") + 1 : -1]
            end = source.find('"' + hashes, match.end())
            yield start, source[match.end() : end]
            i = end + 1 + len(hashes)
        else:
            string = STRING_BODY.match(source, match.end() - 1)
            yield start, STRING_ESCAPE.sub(decode_escape, string.group(1))
            i = string.end()


def is_test_source(relative: Path) -> bool:
    stem = relative.stem
    return (
        "tests" in relative.parts
        or "test_support" in relative.parts
        or stem in ("tests", "test", "test_support")
        or stem.endswith("_tests")
        or stem.startswith("test_")
    )


def file_literals(relative: str, source: str) -> list[Literal]:
    test_module = re.search(r"#\[cfg\(test\)\]\s*mod\s+\w+\s*\{", source)
    lines = source.splitlines()
    newlines = [match.start() for match in re.finditer("\n", source)]
    literals = []
    for offset, text in rust_string_literals(source):
        if test_module and offset > test_module.start():
            break
        line = bisect.bisect_left(newlines, offset) + 1
        window = lines[max(0, line - 1 - EXEMPT_WINDOW) : line]
        literals.append(Literal(relative, line, text, any(EXEMPT.search(row) for row in window)))
    return literals


def source_literals(root: Path) -> list[Literal]:
    literals = []
    for path in sorted(root.glob("crates/*/src/**/*")):
        relative = path.relative_to(root)
        if is_test_source(relative):
            continue
        if path.suffix == ".rs":
            literals += file_literals(str(relative), path.read_text(encoding="utf-8"))
        elif path.suffix == ".sql":
            literals.append(Literal(str(relative), 1, path.read_text(encoding="utf-8"), False))
    return literals


def tokens(sql: str) -> list[tuple[str, str]]:
    return [
        (match.lastgroup, match.group())
        for match in TOKEN.finditer(sql)
        if match.lastgroup not in ("ws", "lc", "bc")
    ]


def identifier(token: tuple[str, str] | None) -> str | None:
    if token is None:
        return None
    kind, text = token
    if kind == "id":
        return text.lower()
    if kind == "qid":
        return text[1:-1].lower()
    return None


def term_ending_at(toks: list[tuple[str, str]], end: int) -> str | None:
    """Normalized column or function-call operand whose last token is toks[end]."""
    if end < 0:
        return None
    if toks[end][1] != ")":
        return identifier(toks[end])
    depth, j = 0, end
    while j >= 0:
        depth += {")": 1, "(": -1}.get(toks[j][1], 0)
        if depth == 0:
            break
        j -= 1
    if j < 1 or not identifier(toks[j - 1]) or toks[j - 1][1].upper() in CLAUSE_KEYWORDS | {"IN"}:
        return None
    return normalized(toks[j - 1 : end + 1])


def term_starting_at(toks: list[tuple[str, str]], start: int) -> str | None:
    """Normalized column or function-call operand whose first token is toks[start]."""
    if start >= len(toks) or not identifier(toks[start]):
        return None
    if start + 1 >= len(toks) or toks[start + 1][1] != "(":
        return identifier(toks[start])
    depth = 0
    for end in range(start + 1, len(toks)):
        depth += {"(": 1, ")": -1}.get(toks[end][1], 0)
        if depth == 0:
            return normalized(toks[start : end + 1])
    return None


def normalized(toks: list[tuple[str, str]]) -> str:
    return "".join(text if kind == "str" else text.lower() for kind, text in toks)


def literal_compared_terms(predicate: str) -> set[str]:
    """Columns or expressions an index predicate compares with a literal.

    `IS [NOT] NULL` terms are left out: a bound comparison still implies them.
    """
    found = set()
    toks = tokens(predicate)
    for k in range(1, len(toks) - 1):
        operator, rest = toks[k][1].upper(), toks[k + 1 :]
        left = k - 1
        if operator == "NOT" and rest[0][1].upper() == "IN":
            operator, rest = "IN", rest[1:]
        if operator == "IN" and rest and rest[0][1] == "(":
            rest = rest[1:]
        elif operator not in COMPARISON:
            continue
        if toks[left][1].upper() == "NOT":
            continue
        term = term_ending_at(toks, left)
        if term and rest and rest[0][0] in ("str", "num"):
            found.add(term)
    return found


def partial_indexes(literals: list[Literal]) -> list[PartialIndex]:
    indexes = {}
    for literal in literals:
        for match in INDEX.finditer(literal.text):
            terms = literal_compared_terms(match.group("predicate"))
            if terms:
                index = PartialIndex(match.group("name").lower(), match.group("table").lower(), frozenset(terms))
                indexes[(index.table, index.name)] = index
    return sorted(indexes.values(), key=lambda index: (index.table, index.name))


def assignment_targets(toks: list[tuple[str, str]]) -> set[int]:
    """Token positions of `col` in `SET col = ...` assignments."""
    targets = set()
    depth, set_depth = 0, None
    for k, (_, text) in enumerate(toks):
        upper = text.upper()
        if text == "(":
            depth += 1
        elif text == ")":
            depth -= 1
            if set_depth is not None and depth < set_depth:
                set_depth = None
        elif upper == "SET":
            set_depth = depth
        elif set_depth == depth and upper in ("WHERE", "FROM", "RETURNING", ";"):
            set_depth = None
        if set_depth == depth and k + 2 < len(toks) and toks[k + 2][1] == "=":
            if upper == "SET" or text == ",":
                targets.add(k + 1)
    return targets


def compared_term(toks: list[tuple[str, str]], k: int) -> str | None:
    """Column or expression a parameter at toks[k] is compared with, or None."""
    previous = toks[k - 1][1].upper() if k else ""
    if previous in COMPARISON:
        return term_ending_at(toks, k - 2)
    if previous == "NOT" and k >= 3 and toks[k - 2][1].upper() == "IS":
        return term_ending_at(toks, k - 3)
    if previous == "BETWEEN":
        return term_ending_at(toks, k - 2)
    if previous == "AND" and k >= 4 and toks[k - 3][1].upper() == "BETWEEN":
        return term_ending_at(toks, k - 4)
    if previous in ("(", ","):
        depth, j = 0, k - 1
        while j >= 0:
            if toks[j][1] == ")":
                depth += 1
            elif toks[j][1] == "(":
                if depth == 0:
                    break
                depth -= 1
            j -= 1
        if j >= 2 and toks[j - 1][1].upper() == "IN":
            before = j - 2
            if toks[before][1].upper() == "NOT":
                before -= 1
            return term_ending_at(toks, before)
    if k + 2 < len(toks) and toks[k + 1][1].upper() in COMPARISON:
        return term_starting_at(toks, k + 2)
    return None


def binds_like_pattern(toks: list[tuple[str, str]], k: int) -> bool:
    """`LIKE ?` or `GLOB ?` with the bare parameter as the whole pattern."""
    if k == 0 or toks[k - 1][1].upper() not in ("LIKE", "GLOB"):
        return False
    if k >= 2 and toks[k - 2][1].upper() == "NOT":
        return False
    return not (k + 1 < len(toks) and toks[k + 1][1] == "||")


def table_after(toks: list[tuple[str, str]], k: int) -> tuple[str | None, int]:
    """Table named at toks[k] (`schema.table` allowed) and the position after it."""
    if k + 2 < len(toks) and toks[k + 1][1] == ".":
        k += 2
    return identifier(toks[k]) if k < len(toks) else None, k + 1


def visible_tables(toks: list[tuple[str, str]]) -> list[frozenset[str]]:
    """Tables each token's query block and its enclosing blocks read or write."""
    blocks: list[set[str]] = [set()]
    parents = [-1]
    stack = [0]
    parens: list[bool] = []
    block_of = []
    for k, (_, text) in enumerate(toks):
        upper = text.upper()
        if text == "(":
            opens = k + 1 < len(toks) and toks[k + 1][1].upper() in ("SELECT", "WITH", "VALUES")
            parens.append(opens)
            if opens:
                blocks.append(set())
                parents.append(stack[-1])
                stack.append(len(blocks) - 1)
        elif text == ")" and parens:
            if parens.pop():
                stack.pop()
        block_of.append(stack[-1])
        if upper not in ("FROM", "JOIN", "UPDATE", "INTO"):
            continue
        j = k + 1
        if upper == "UPDATE" and j < len(toks) and toks[j][1].upper() == "OR":
            j += 2
        while j < len(toks):
            table, j = table_after(toks, j)
            if table is None:
                break
            blocks[stack[-1]].add(table)
            if upper != "FROM":
                break
            if j < len(toks) and toks[j][1].upper() == "AS":
                j += 1
            if j < len(toks) and identifier(toks[j]) and toks[j][0] != "param" and toks[j][1].upper() not in CLAUSE_KEYWORDS:
                j += 1
            if j >= len(toks) or toks[j][1] != ",":
                break
            j += 1
    visible = []
    for block in range(len(blocks)):
        tables, parent = set(blocks[block]), parents[block]
        while parent >= 0:
            tables |= blocks[parent]
            parent = parents[parent]
        visible.append(frozenset(tables))
    return [visible[block] for block in block_of]


def check_statement(literal: Literal, guarded: dict[str, dict[str, list[str]]]) -> list[Finding]:
    toks = tokens(literal.text)
    scopes = visible_tables(toks)
    assigned = assignment_targets(toks)
    findings = []
    for k, (kind, text) in enumerate(toks):
        if kind != "param":
            continue
        if binds_like_pattern(toks, k):
            findings.append(
                Finding(
                    literal.path,
                    literal.line,
                    f"binds {toks[k - 1][1].upper()} pattern {text}; a bound pattern loses the prefix-range plan",
                )
            )
            continue
        term = compared_term(toks, k)
        if term is None or (k >= 2 and k - 2 in assigned):
            continue
        indexes = sorted(name for table in scopes[k] for name in guarded.get(table, {}).get(term, ()))
        if indexes:
            findings.append(
                Finding(
                    literal.path,
                    literal.line,
                    f"binds {text} against `{term}`, a predicate term of partial index "
                    f"{', '.join(indexes)}; name the predicate literal instead",
                )
            )
    return findings


def check(root: Path) -> tuple[list[PartialIndex], list[Finding]]:
    literals = source_literals(root)
    indexes = partial_indexes(literals)
    guarded: dict[str, dict[str, list[str]]] = {}
    for index in indexes:
        for term in index.terms:
            guarded.setdefault(index.table, {}).setdefault(term, []).append(index.name)
    findings = []
    for literal in literals:
        if literal.exempt or not SQL_HEAD.match(literal.text):
            continue
        findings += check_statement(literal, guarded)
    return indexes, findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    indexes, findings = check(args.root)
    if not indexes:
        print("no partial index with a literal predicate found; the source scan is broken", file=sys.stderr)
        return 1
    for finding in findings:
        print(finding)
    if findings:
        print(
            f"{len(findings)} bound value(s) SQLite cannot plan with under QPSG. "
            "Name the literal, or add `// qpsg-exempt: <reason>` above the SQL.",
            file=sys.stderr,
        )
        return 1
    print(f"checked {len(indexes)} partial indexes; no bound predicate or LIKE pattern")
    return 0


if __name__ == "__main__":
    sys.exit(main())
