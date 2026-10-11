#!/usr/bin/env python3
"""Rewrites test path constructions that spell an owned-store file name onto
`tracedecay_store::StoreFile`, so the same test names `<stem>.db` under the
SQLite engine and `<stem>.tdb` under the native one.

Covered shapes, where NAME is a legacy main file such as `global.db`:
  .join("NAME") / .join("dir/NAME")
  PathBuf::from("NAME") / PathBuf::from("dir/NAME") / Path::new("NAME")
  .join(format!("dir/{x}/NAME")) / PathBuf::from(format!("dir/{x}/NAME"))
  format!("dir/{x}/NAME", ...) and "dir/NAME" as a value
  "NAME" as a whole literal

Assertion messages, `.contains("...")` text checks, raw strings, comments and
sidecar names (`-wal`, `-shm`, `.host-admission`) are left for review by hand. Run from the repository root
with the crate directories to rewrite.
"""

import pathlib
import re
import sys

FAMILIES = {
    "global": "Profile",
    "user-memory": "ProfileMemory",
    "user-sessions": "ProfileSessions",
    "remote": "RemoteNode",
    "tracedecay": "Project",
    "sessions": "ProjectSessions",
}
STEM = "(?P<stem>" + "|".join(re.escape(stem) for stem in FAMILIES) + r")\.db"
PREFIX = r"(?P<pre>/?[A-Za-z0-9_.{}-]+(?:/[A-Za-z0-9_.{}-]+)*)"
PLAIN_PREFIX = r"(?P<pre>/?[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*)"


def store(match: re.Match) -> str:
    return f"tracedecay_store::StoreFile::{FAMILIES[match['stem']]}.file_name()"


REWRITES = [
    # .join("dir/NAME") and PathBuf::from("dir/NAME")
    (
        re.compile(rf'(?P<call>\.join|PathBuf::from)\("{PLAIN_PREFIX}/{STEM}"\)'),
        lambda m: f'{m["call"]}("{m["pre"]}").join({store(m)})',
    ),
    # .join(format!("dir/{x}/NAME", args)) and PathBuf::from(format!(...))
    (
        re.compile(
            rf'(?P<call>\.join|PathBuf::from)\(format!\("{PREFIX}/{STEM}"(?P<args>[^()]*)\)\)'
        ),
        lambda m: f'{m["call"]}(format!("{m["pre"]}"{m["args"]})).join({store(m)})',
    ),
    # format!("dir/{x}/NAME", args) as a value
    (
        re.compile(rf'format!\("{PREFIX}/{STEM}"(?P<args>[^()]*)\)'),
        lambda m: f'format!("{m["pre"]}/{{}}"{m["args"]}, {store(m)})',
    ),
    # "dir/NAME".to_string() and "dir/NAME" as a value
    (
        re.compile(rf'"{PLAIN_PREFIX}/{STEM}"(?:\.to_string\(\)|\.to_owned\(\)|\.into\(\))?'),
        lambda m: f'format!("{m["pre"]}/{{}}", {store(m)})',
    ),
    # "NAME" as a whole literal
    (re.compile(rf'"{STEM}"'), store),
]


def rewrite_line(line: str) -> str:
    stripped = line.lstrip()
    # `.contains("...")` checks rendered text, such as guidance that must not
    # name a retired path, so its literal stays as written.
    if stripped.startswith("//") or 'r#"' in line or '.contains("' in line:
        return line
    for pattern, replacement in REWRITES:
        line = pattern.sub(replacement, line)
    return line


def main(roots: list[str]) -> int:
    changed = 0
    for root in roots:
        for path in sorted(pathlib.Path(root).rglob("*.rs")):
            text = path.read_text()
            updated = "".join(rewrite_line(line) for line in text.splitlines(keepends=True))
            if updated != text:
                path.write_text(updated)
                changed += 1
                print(path)
    print(f"{changed} files rewritten", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
