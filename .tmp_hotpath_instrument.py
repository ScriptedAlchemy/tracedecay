#!/usr/bin/env python3
import json
import subprocess
import sys
from collections import defaultdict
from pathlib import Path


EXCLUDED = ("/tests/", "_tests.rs", "/tests.rs", "/test_support.rs", "generated")


def search(path_glob: str, pattern: str) -> list[dict]:
    args = {
        "pattern": pattern,
        "lang": "rust",
        "path_glob": path_glob,
        "max_results": 200,
        "format": "json",
    }
    result = subprocess.run(
        ["tracedecay", "tool", "ast_grep_search", "--args", json.dumps(args), "--json"],
        capture_output=True,
        text=True,
    )
    if result.returncode:
        return []
    envelope = json.loads(result.stdout)
    payload = json.loads(envelope["content"][0]["text"])
    if payload.get("truncated"):
        raise RuntimeError(f"search truncated for {path_glob}: {pattern}")
    return payload["results"]


def test_only(lines: list[str], index: int) -> bool:
    context = "\n".join(lines[max(0, index - 8) : index])
    return "#[cfg" in context and any(
        marker in context for marker in ("test", "test-helpers", "eval-helpers")
    )


def add_impls(path_glob: str) -> tuple[int, int]:
    by_file: dict[str, list[int]] = defaultdict(list)
    for path in sorted(Path(".").glob(path_glob)):
        raw = path.as_posix()
        if any(x in raw for x in EXCLUDED):
            continue
        for result in search(raw, "impl $T { $$$ }"):
            if result["column"] == 1:
                by_file[result["file"]].append(result["line"] - 1)
    return insert(by_file, "#[hotpath::measure_all]\n")


def add_functions(path_glob: str) -> tuple[int, int]:
    changed = added = 0
    for path in sorted(Path(".").glob(path_glob)):
        raw = path.as_posix()
        if any(x in raw for x in EXCLUDED):
            continue
        results: list[dict] = []
        for pattern in (
            "fn $F($$$ARGS) { $$$BODY }",
            "fn $F($$$ARGS) -> $R { $$$BODY }",
        ):
            results.extend(search(raw, pattern))
        indices = {
            result["line"] - 1
            for result in results
            if result["column"] == 1 and "const fn" not in result["line_text"]
        }
        files, attrs = insert({raw: list(indices)}, "#[hotpath::measure]\n")
        changed += files
        added += attrs
    return changed, added


def insert(by_file: dict[str, list[int]], attribute: str) -> tuple[int, int]:
    files_changed = attributes_added = 0
    for raw, indices in by_file.items():
        path = Path(raw)
        lines = path.read_text().splitlines(keepends=True)
        changed = False
        for index in sorted(set(indices), reverse=True):
            context = "\n".join(lines[max(0, index - 8) : index])
            if "hotpath::measure" in context or test_only(lines, index):
                continue
            lines.insert(index, attribute)
            attributes_added += 1
            changed = True
        if changed:
            path.write_text("".join(lines))
            files_changed += 1
    return files_changed, attributes_added


def add_const_skips(path_glob: str) -> tuple[int, int]:
    changed = added = 0
    for path in sorted(Path(".").glob(path_glob)):
        raw = path.as_posix()
        if any(x in raw for x in EXCLUDED):
            continue
        lines = path.read_text().splitlines(keepends=True)
        indices = [
            index
            for index, line in enumerate(lines)
            if line.startswith("    ") and " const fn " in line
        ]
        file_changed = False
        for index in reversed(indices):
            context = "\n".join(lines[max(0, index - 4) : index])
            if "hotpath::skip" in context or test_only(lines, index):
                continue
            lines.insert(index, "    #[hotpath::skip]\n")
            added += 1
            file_changed = True
        if file_changed:
            path.write_text("".join(lines))
            changed += 1
    return changed, added


if __name__ == "__main__":
    glob, mode = sys.argv[1:3]
    if mode == "functions":
        changed, added = add_functions(glob)
    elif mode == "const-skips":
        changed, added = add_const_skips(glob)
    else:
        changed, added = add_impls(glob)
    print(f"files_changed={changed} attributes_added={added}")
