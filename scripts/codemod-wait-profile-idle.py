#!/usr/bin/env python3
"""Rewrite `assert!(registry.wait_profile_idle(path, budget).await, ...)` to the
unbounded event-driven `registry.wait_profile_idle(path).await;`."""

import pathlib
import re
import sys

PATTERN = re.compile(
    r"assert!\(\n\s+(\w+)\n\s+\.wait_profile_idle\(([^\n]+?), Duration::from_\w+\(\d+\)\)\n"
    r"\s+\.await,?\n(?:\s+\"[^\"\n]*\"\n)?(\s*)\);"
)

total = 0
for name in sys.argv[1:]:
    path = pathlib.Path(name)
    text = path.read_text()
    rewritten, count = PATTERN.subn(
        lambda m: f"{m.group(1)}.wait_profile_idle({m.group(2)}).await;", text
    )
    if count:
        path.write_text(rewritten)
    print(f"{name}: {count}")
    total += count
print(f"total: {total}")
