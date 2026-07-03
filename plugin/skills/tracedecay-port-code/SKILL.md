---
name: tracedecay-port-code
description: 'Use to port or migrate code between directories in dependency-safe order and track progress.'
---

# Port code

Use when asked to port or migrate code between directories in dependency-safe order and track progress.

Route this through the `tracedecay:editing-safely` skill, using the port tools (`tracedecay_port_order`, `tracedecay_port_status`).

- **Args:** "<source_dir> <target_dir>". If absent, ask for the source and target directories.
- Follow that skill's dependency-safe workflow and guardrails: port leaves first, and confirm before edits and toolchain runs.

Output: updated port status (done / remaining) and the per-batch typecheck result.
