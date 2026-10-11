#!/usr/bin/env python3
"""Lists non-test Rust lines that spell an owned-store file name outside
`tracedecay-store/src/runtime/store_file.rs`. Test files, `tests/` trees,
benches and everything after a file's first `#[cfg(test)]` module are
skipped. Exits non-zero when any line is found.
"""

import pathlib
import re
import sys

OWNER = pathlib.Path("crates/tracedecay-store/src/runtime/store_file.rs")
STEMS = "global|user-memory|user-sessions|remote|tracedecay|sessions"
NAME = re.compile(rf'"(?:\.)?(?:{STEMS})\.(?:db|tdb|grafeo)[^"]*"')
TEST_MODULE = re.compile(
    r"^\s*#\[cfg\((?:test|all\(test[^\]]*)\)\]\s*\n\s*(?:#\[[^\]]*\]\s*\n\s*)*(?:pub(?:\([a-z]+\))? )?mod ", re.M
)
# Modules declared `#[cfg(test)] mod name;` from their parent.
TEST_ONLY_FILES = {
    "crates/tracedecay-daemon-service/src/retained_owner/memory_target_journeys.rs",
    "crates/tracedecay-global-db/src/schema_contract/invariants/test_fixture.rs",
}


def production_text(path: pathlib.Path) -> str:
    text = path.read_text()
    match = TEST_MODULE.search(text)
    return text[: match.start()] if match else text


def main() -> int:
    hits = []
    for path in sorted(pathlib.Path("crates").rglob("*.rs")):
        parts = set(path.parts)
        name = path.name
        if path == OWNER or str(path) in TEST_ONLY_FILES:
            continue
        if parts & {"tests", "benches", "test_support"}:
            continue
        if name.endswith(("_test.rs", "_tests.rs")) or name in {"tests.rs", "test_support.rs"}:
            continue
        for number, line in enumerate(production_text(path).splitlines(), 1):
            if line.lstrip().startswith("//"):
                continue
            if NAME.search(line):
                hits.append(f"{path}:{number}: {line.strip()}")
    print("\n".join(hits))
    return 1 if hits else 0


if __name__ == "__main__":
    sys.exit(main())
