---
description: Port or migrate code between directories in dependency-safe order and track progress.
argument-hint: "[source_dir target_dir]"
---

# Port code

Interpret `$ARGUMENTS` as `<source_dir> <target_dir>`; if either is missing, ask
for it. Use `tracedecay_port_status` and `tracedecay_port_order` to move leaf
dependencies before their dependents. For each selected symbol, inspect its body,
contract, callers, and callees, then use anchored edits.

After a coherent batch, refresh port status and run the native typecheck;
retained diagnostics are not fresh verification. Respect the host's approval and
run mode for edits and toolchain execution. Continue through the requested port
unless a missing decision or failed check blocks it, and report remaining work.
