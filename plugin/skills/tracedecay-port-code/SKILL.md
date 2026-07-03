---
name: tracedecay-port-code
description: 'Use to port or migrate code between directories in dependency-safe order and track progress.'
---

# Port code

Use to port or migrate code between directories in dependency-safe order and track progress.

Use `tracedecay:editing-safely` with `tracedecay_port_order` and `tracedecay_port_status`.

- **Args:** "<source_dir> <target_dir>". If absent, ask for the source and target directories.
- Port leaves first. Confirm before edits and toolchain runs.

Output: updated port status (done / remaining) and the per-batch typecheck result.
