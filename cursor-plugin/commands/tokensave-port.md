---
name: tokensave-port
description: Port/migrate code between directories in dependency-safe order and track progress.
---

# /tokensave-port

Apply the `tokensave:porting-code` skill.

- **Args:** interpret `$ARGUMENTS` as "<source_dir> <target_dir>"; if absent, ask for the source and target directories.
- Follow that skill's dependency-safe workflow and guardrails (port leaves first; only apply edits / run the toolchain when the user wants it; respect Cursor approval/run-mode).

Output: updated port status (done / remaining) and the per-batch typecheck result.
