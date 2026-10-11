#!/usr/bin/env python3
"""Qualify every `DROP TRIGGER` in the given Rust or SQL files with `main.`.

The native engine resolves an unqualified trigger name against temp first,
so it refuses unqualified drops; `main.` means the same thing on SQLite.
Usage: qualify-trigger-drops.py FILE...
"""

import re
import sys
from pathlib import Path

DROP = re.compile(r"(DROP TRIGGER (?:IF EXISTS )?)(?!main\.)")

for name in sys.argv[1:]:
    path = Path(name)
    text = path.read_text()
    updated = DROP.sub(r"\1main.", text)
    if updated != text:
        path.write_text(updated)
        print(f"{name}: {len(DROP.findall(text))}")
