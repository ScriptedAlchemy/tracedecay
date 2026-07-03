---
description: Port or migrate code between directories in dependency-safe order and track progress.
argument-hint: "[source_dir target_dir]"
---

# Port code

Interpret `$ARGUMENTS` as "<source_dir> <target_dir>". If absent, ask for the source and target directories. Port leaves first; never port a symbol before its dependencies.

1. Baseline → `tracedecay_port_status` (`source_dir`, `target_dir`, `kinds`); order → `tracedecay_port_order`: topological sort — port leaves first, dependents after.
2. Per symbol: pull source with `tracedecay_body`, map dependencies with `tracedecay_callees` / `tracedecay_callers`, confirm the contract with `tracedecay_signature`, apply with the anchored edit primitives (`tracedecay_str_replace`, `tracedecay_insert_at`, `tracedecay_replace_symbol`).
3. After each batch: re-run `tracedecay_port_status`; typecheck with `tracedecay_diagnostics`. Cross-branch parity → `tracedecay_branch_diff` / `tracedecay_changelog`.

Edit primitives and toolchain runs mutate state — respect approval/run-mode.

Output: updated port status (done / remaining) and the per-batch typecheck result. If any result includes a `tracedecay_metrics:` line, report the savings.
