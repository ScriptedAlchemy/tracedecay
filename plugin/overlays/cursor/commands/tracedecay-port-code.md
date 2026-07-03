---
description: Port or migrate code between directories in dependency-safe order and track progress.
---

# /tracedecay-port-code

Apply the `tracedecay:editing-safely` skill.

- **Args:** interpret `$ARGUMENTS` as "<source_dir> <target_dir>"; if absent, ask for the source and target directories.
- Follow that skill's dependency-safe workflow and guardrails (port leaves first; respect Cursor approval/run-mode for edits and toolchain runs).

Output: updated port status (done / remaining) and the per-batch typecheck result.
