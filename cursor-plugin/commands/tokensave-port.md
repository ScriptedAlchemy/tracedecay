---
name: tokensave-port
description: Port/migrate code between directories in dependency-safe order and track progress.
---

# /tokensave-port

Apply the `tokensave:porting-code` skill. Interpret `$ARGUMENTS` as "<source_dir> <target_dir>" when present; otherwise ask for the source and target directories.

Steps:
1. `tokensave_port_status` (source vs target) for the baseline.
2. `tokensave_port_order` for a dependency-safe order (leaves first).
3. Per symbol: `tokensave_body` / `tokensave_callees` / `tokensave_callers` / `tokensave_signature`, then apply the port.
4. After each batch: re-run `tokensave_port_status` and `tokensave_diagnostics`. Only apply edits / run the toolchain when the user wants them; respect Cursor approval/run-mode.

Output: updated port status (done / remaining) and per-batch typecheck result. Report any `tokensave_metrics:` savings to the user.
