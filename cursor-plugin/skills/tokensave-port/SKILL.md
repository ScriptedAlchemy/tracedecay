---
name: tokensave-port
description: Port/migrate code between directories in dependency-safe order and track progress.
disable-model-invocation: true
---

# /tokensave-port

Apply the `tokensave:porting-code` skill.

- **Args:** interpret the text after the command as "<source_dir> <target_dir>"; if absent, ask for the source and target directories.
- Follow that skill's dependency-safe workflow and guardrails (port leaves first; respect Cursor approval/run-mode for edits and toolchain runs).

Output: updated port status (done / remaining) and the per-batch typecheck result.
