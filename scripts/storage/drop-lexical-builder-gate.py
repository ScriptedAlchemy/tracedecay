#!/usr/bin/env python3
"""Deletes the lexical artifact's private-builder gate from builder.rs.

The gate was a custom SQLite scalar function consulted by `builder_gate_*`
triggers. The native engine registers no custom functions and holds the one
open of a store file, so the gate goes: this removes every
`*BUILDER_GATE_TRIGGER_LAYOUT` constant (with its doc comment), the
`BUILDER_MUTATION_*` constants and their comment, and every
`CREATE TRIGGER builder_gate_*` DDL line. Idempotent.

Usage: python3 scripts/storage/drop-lexical-builder-gate.py
"""

import re
from pathlib import Path

BUILDER = (
    Path(__file__).resolve().parent.parent.parent
    / "crates/tracedecay-query/src/retrieval/lexical/projection/artifact/builder.rs"
)


def drop_items(lines):
    kept = []
    index = 0
    while index < len(lines):
        line = lines[index]
        gate_const = re.match(r"const \w*BUILDER_GATE_TRIGGER_LAYOUT\b", line)
        mutation_const = re.match(r"const BUILDER_MUTATION_\w+: ", line)
        if gate_const or mutation_const:
            while kept and kept[-1].lstrip().startswith("//"):
                kept.pop()
            if gate_const:
                while not lines[index].rstrip().endswith(("];", ")];")):
                    index += 1
            index += 1
            continue
        if "CREATE TRIGGER builder_gate_" in line:
            index += 1
            continue
        kept.append(line)
        index += 1
    return kept


def main():
    original = BUILDER.read_text().splitlines(keepends=True)
    patched = drop_items(original)
    if patched != original:
        BUILDER.write_text("".join(patched))
    print(f"drop-lexical-builder-gate: removed {len(original) - len(patched)} line(s)")


if __name__ == "__main__":
    main()
