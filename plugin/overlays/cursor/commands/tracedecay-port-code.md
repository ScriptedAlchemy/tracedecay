---
name: tracedecay-port-code
description: Port or migrate code between directories in dependency-safe order and track progress.
---

# /tracedecay-port-code

Use `tracedecay:editing-safely`.

Interpret `$ARGUMENTS` as `<source_dir> <target_dir>`; ask for a missing side.
Use port order to move leaf dependencies before dependents. Respect Cursor
approval and run mode for edits and toolchain checks. Continue through the
requested port unless a missing decision or failed check blocks it, then report
verified progress and remaining work.
