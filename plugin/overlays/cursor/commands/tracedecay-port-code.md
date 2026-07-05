---
name: tracedecay-port-code
description: Port or migrate code between directories in dependency-safe order and track progress.
---

# /tracedecay-port-code

Use `tracedecay:editing-safely`.

- **Args:** interpret `$ARGUMENTS` as "<source_dir> <target_dir>"; if absent, ask for the source and target directories.
- Port leaves first. Respect Cursor approval/run-mode for edits and toolchain runs.

Output: updated port status (done / remaining) and the per-batch typecheck result.
