#!/usr/bin/env python3
"""Keep production code off the ambient Tokio runtime.

`tracedecay daemon run` drives its futures on native Asupersync workers, which
have no ambient Tokio runtime. A call that looks one up panics there ("there is
no reactor running") or reaches a runtime that is already gone. Production code
reaches Tokio only through `native_runtime::tokio_handle`, `on_tokio` or an
explicit `Handle`/`spawn_on` (#3940).

This check reads `crates/*/src` (test files, `#[cfg(test)]` items and
test-helper features excluded), resolves each file's `use tokio::...` imports,
and reports a reference to:

- `tokio::spawn`, `tokio::task::spawn`, `tokio::task::spawn_blocking`
- `tokio::runtime::Handle::current`
- `tokio::time::{sleep, sleep_until, timeout, timeout_at, interval, interval_at}`
- `JoinSet::spawn` and `JoinSet::spawn_blocking`, including a method call on a
  variable or field the same file declares as Tokio's `JoinSet`

A deliberate exception names its reason in a comment on the call's line or one
of the three lines above it, or in the comment block right above a `mod x;`
line to cover that whole file:

    // ambient-tokio: <why this code always runs inside a Tokio context>

    scripts/check-ambient-tokio.py [--root <repo>]
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

BANNED = {
    "tokio::spawn",
    "tokio::task::spawn",
    "tokio::task::spawn_blocking",
    "tokio::runtime::Handle::current",
    "tokio::task::JoinSet::spawn",
    "tokio::task::JoinSet::spawn_blocking",
    *(f"tokio::time::{name}" for name in ("sleep", "sleep_until", "timeout", "timeout_at", "interval", "interval_at")),
}
JOIN_SET = "tokio::task::JoinSet"
EXEMPT = re.compile(r"//\s*ambient-tokio:\s*\S")
EXEMPT_WINDOW = 3
LEXEME = re.compile(r"""//|/\*|(?<![\w])b?r#*"|(?<![\w])b?"|'""")
BLOCK_COMMENT = re.compile(r"/\*|\*/")
CHAR_LITERAL = re.compile(r"'(?:\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'\n])'")
STRING_BODY = re.compile(r'"(?:\\.|[^"\\])*"', re.S)
TEST_CFG = re.compile(
    r'#\[cfg\((?:test|all\(\s*test\s*,[^\]]*\)|any\((?:\s*(?:test|feature\s*=\s*"test[-\w]*")\s*,?)+\))\)\]'
)
ATTRIBUTE = re.compile(r"\s*#\[[^\]]*\]")
PATH_ATTRIBUTE = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')
EXTERNAL_MOD = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")
DECLARED_MOD = re.compile(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?mod[ \t]+(\w+)[ \t]*;")
USE = re.compile(r"\buse\s+(::)?([^;]+);")
USE_TOKEN = re.compile(r"\w+|::|[{},*]")
METHOD = re.compile(r"\b(\w+)\s*\.\s*(spawn|spawn_blocking)\s*(?:::<[^>]*>\s*)?\(")
JOIN_SET_BINDING = re.compile(r"\b(\w+)\s*(?::\s*(?:[\w:]+::)?JoinSet\b|=\s*(?:[\w:]+::)?JoinSet\s*::)")


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    target: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: ambient `{self.target}`"


def blank(text: str) -> str:
    return "\n".join(" " * len(part) for part in text.split("\n"))


def strip_comments_and_strings(source: str) -> str:
    """Source with comments and string or char literals blanked, newlines kept."""
    out, i = [], 0
    while match := LEXEME.search(source, i):
        start, lexeme = match.start(), match.group()
        out.append(source[i:start])
        if lexeme == "//":
            end = source.find("\n", start)
            end = len(source) if end < 0 else end
        elif lexeme == "/*":
            depth, end = 1, start + 2
            while depth and (close := BLOCK_COMMENT.search(source, end)):
                depth += 1 if close.group() == "/*" else -1
                end = close.end()
            if depth:
                end = len(source)
        elif lexeme == "'":
            char = CHAR_LITERAL.match(source, start)
            end = char.end() if char else start + 1
        elif "r" in lexeme:
            hashes = lexeme[lexeme.index("r") + 1 : -1]
            close = source.find('"' + hashes, match.end())
            end = len(source) if close < 0 else close + 1 + len(hashes)
        else:
            string = STRING_BODY.match(source, match.end() - 1)
            end = string.end() if string else len(source)
        out.append(blank(source[start:end]))
        i = end
    out.append(source[i:])
    return "".join(out)


def item_end(code: str, start: int) -> int:
    """End of the item starting at `start`: its first `;` or its brace block."""
    depth = 0
    for k in range(start, len(code)):
        if code[k] == "{":
            depth += 1
        elif code[k] == "}":
            depth -= 1
            if depth == 0:
                return k + 1
        elif code[k] == ";" and depth == 0:
            return k + 1
    return len(code)


def strip_test_items(source: str, code: str, path: Path) -> tuple[str, list[Path], list[Path]]:
    """Blank `#[cfg(test)]` items.

    Returns the code, the files that test-only `mod x;` lines load, and the
    files whose `mod x;` line carries an `ambient-tokio:` reason.
    """
    directory = module_directory(path)

    def module_files(name: str, attributes: str) -> list[Path]:
        if explicit := PATH_ATTRIBUTE.search(attributes):
            return [path.parent / explicit.group(1)]
        return [directory / f"{name}.rs", directory / name / "mod.rs"]

    pieces, i, test_files = [], 0, []
    for match in TEST_CFG.finditer(source):
        start, after = match.start(), match.end()
        if start < i or code[start] != "#":
            continue
        while attribute := ATTRIBUTE.match(code, after):
            after = attribute.end()
        if external := EXTERNAL_MOD.match(code, after):
            test_files += module_files(external.group(1), source[start:after])
        end = item_end(code, after)
        pieces += [code[i:start], blank(code[start:end])]
        i = end
    code = "".join(pieces) + code[i:]
    lines = source.splitlines()
    exempt_files = []
    for external in DECLARED_MOD.finditer(code):
        above = code.count("\n", 0, external.end())
        while above and lines[above - 1].lstrip().startswith(("//", "#[")):
            above -= 1
        window = "\n".join(lines[above : code.count("\n", 0, external.end()) + 1])
        if EXEMPT.search(window):
            exempt_files += module_files(external.group(1), window)
    return code, test_files, exempt_files


def use_tree(tokens: list[str], k: int, prefix: list[str], aliases: dict[str, str], globs: set[str]) -> int:
    """Parse one use tree at tokens[k] under `prefix`; return the next index."""
    path = list(prefix)
    while k < len(tokens):
        token = tokens[k]
        if token == "{":
            k += 1
            while k < len(tokens) and tokens[k] != "}":
                k = use_tree(tokens, k, path, aliases, globs)
                if k < len(tokens) and tokens[k] == ",":
                    k += 1
            return k + 1
        if token == "*":
            globs.add("::".join(path))
            return k + 1
        if token in (",", "}"):
            break
        if token != "::":
            path.append(token)
        k += 1
        if k + 1 < len(tokens) and tokens[k] == "as":
            if tokens[k + 1] != "_":
                aliases[tokens[k + 1]] = "::".join(path)
            return k + 2
    if path and path[-1] == "self":
        path.pop()
    if len(path) > len(prefix) or (path and path == prefix):
        aliases[path[-1]] = "::".join(path)
    return k


def tokio_imports(code: str) -> tuple[dict[str, str], set[str], str]:
    """Tokio aliases and glob modules from `use` lines, and code with them blanked."""
    aliases: dict[str, str] = {}
    globs: set[str] = set()
    for match in USE.finditer(code):
        use_tree(USE_TOKEN.findall(match.group(2)), 0, [], aliases, globs)
    tokio = {name: path for name, path in aliases.items() if path.split("::")[0] == "tokio"}
    return tokio, {glob for glob in globs if glob.split("::")[0] == "tokio"}, USE.sub(lambda m: blank(m.group()), code)


def resolve(segments: list[str], aliases: dict[str, str], globs: set[str]) -> str | None:
    head, rest = segments[0], segments[1:]
    if head == "tokio":
        return "::".join(segments)
    if head in aliases:
        return "::".join([aliases[head], *rest])
    for glob in globs:
        candidate = "::".join([glob, *segments])
        if candidate in BANNED or candidate == JOIN_SET:
            return candidate
    return None


def check_file(relative: str, source: str, code: str) -> list[Finding]:
    aliases, globs, code = tokio_imports(code)
    hits, uses_join_set = [], False
    heads = {"tokio", *aliases}
    heads.update(target.rsplit("::", 1)[1] for target in (*BANNED, JOIN_SET) for glob in globs if target.startswith(glob + "::"))
    path = re.compile(r"(?<![\w.:])(?:::)?(?:" + "|".join(sorted(heads)) + r")\b(?:\s*::\s*[A-Za-z_]\w*)*")
    for match in path.finditer(code):
        segments = "".join(match.group().split()).lstrip(":").split("::")
        target = resolve(segments, aliases, globs)
        if target == JOIN_SET or (target or "").startswith(JOIN_SET + "::"):
            uses_join_set = True
        if target in BANNED:
            hits.append((match.start(), target))
    if uses_join_set:
        join_sets = set(JOIN_SET_BINDING.findall(code))
        hits += [(m.start(), f"{JOIN_SET}::{m.group(2)}") for m in METHOD.finditer(code) if m.group(1) in join_sets]
    lines = source.splitlines()
    findings = []
    for offset, target in sorted(hits):
        line = code.count("\n", 0, offset) + 1
        if not any(EXEMPT.search(row) for row in lines[max(0, line - 1 - EXEMPT_WINDOW) : line]):
            findings.append(Finding(relative, line, target))
    return findings


def is_test_source(relative: Path) -> bool:
    return any(
        part in ("tests", "test", "test_support")
        or part.startswith("test_")
        or part.endswith(("_tests", "_test", "_test_support"))
        for part in (*relative.parts[:-1], relative.stem)
    )


def module_directory(path: Path) -> Path:
    return path.parent if path.stem in ("mod", "lib", "main") else path.with_suffix("")


def check(root: Path) -> tuple[int, list[Finding]]:
    stripped: dict[Path, tuple[str, str]] = {}
    test_files: set[Path] = set()
    exempt_files: set[Path] = set()
    for path in sorted(root.glob("crates/*/src/**/*.rs")):
        if is_test_source(path.relative_to(root)):
            continue
        source = path.read_text(encoding="utf-8")
        code, tests, exempt = strip_test_items(source, strip_comments_and_strings(source), path)
        stripped[path] = (source, code)
        test_files.update(test.resolve() for test in tests)
        exempt_files.update(file.resolve() for file in exempt)
    skipped = test_files | exempt_files
    skipped_dirs = {module_directory(file) for file in skipped}
    findings = []
    scanned = 0
    for path, (source, code) in stripped.items():
        resolved = path.resolve()
        if resolved in skipped or any(parent in skipped_dirs for parent in resolved.parents):
            continue
        scanned += 1
        findings += check_file(str(path.relative_to(root)), source, code)
    return scanned, findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    scanned, findings = check(args.root)
    if not scanned:
        print("no production Rust source found; the source scan is broken", file=sys.stderr)
        return 1
    for finding in findings:
        print(finding)
    if findings:
        print(
            f"{len(findings)} ambient Tokio call(s) in production code. A native worker has no "
            "ambient Tokio runtime: use native_runtime::tokio_handle, on_tokio or spawn_on, "
            "or add `// ambient-tokio: <reason>` above the call.",
            file=sys.stderr,
        )
        return 1
    print(f"checked {scanned} production source files; no ambient Tokio call")
    return 0


if __name__ == "__main__":
    sys.exit(main())
