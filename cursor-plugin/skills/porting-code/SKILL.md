---
name: porting-code
description: Port or migrate code between directories or languages in dependency-safe order and track progress. Use when porting, migrating, or rewriting a module from one location/language to another (e.g. legacy → new, src → crate).
---

# Porting code

## Workflow

1. **Baseline → `tracedecay_port_status`** (`source_dir`, `target_dir`, `kinds`): what is already ported vs missing.
2. **Order → `tracedecay_port_order`** (`source_dir`, `kinds`, `limit`): topological sort — **port leaves first**, dependents after.
3. **Per symbol, in order:**
   - Pull source with `tracedecay_body` / `tracedecay_node`.
   - Map dependencies with `tracedecay_callees`; map incoming use with `tracedecay_callers`.
   - Confirm the contract with `tracedecay_signature`.
   - Apply the ported code with the `tracedecay:atomic-code-edits` primitives (`tracedecay_replace_symbol` / `tracedecay_str_replace` / `tracedecay_insert_at_symbol`) or your normal edit tools.
4. **After each batch:** re-run `tracedecay_port_status` to update progress; run `tracedecay_diagnostics` to typecheck the target.
5. **Cross-branch parity (if porting across refs):** `tracedecay_branch_diff` / `tracedecay_changelog`.

## Guardrails

- Never port a symbol before its dependencies (respect `tracedecay_port_order`).
- `tracedecay_port_status` / `tracedecay_port_order` and the lookups are read-only; the editing tools and `tracedecay_diagnostics` mutate the working tree / run the toolchain. Use them when porting/verification is relevant and respect Cursor approval/run-mode.
- `tracedecay_diagnostics` forces target dir `.tracedecay/target/`; the first run can take minutes.

## Output

- Updated port status (done / remaining) and the per-batch typecheck result.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
