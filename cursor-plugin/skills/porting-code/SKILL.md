---
name: porting-code
description: Port or migrate code between directories or languages in dependency-safe order and track progress. Use when porting, migrating, or rewriting a module from one location/language to another (e.g. legacy → new, src → crate).
---

# Porting code

## Workflow

1. **Baseline → `tokensave_port_status`** (`source_dir`, `target_dir`, `kinds`): what is already ported vs missing.
2. **Order → `tokensave_port_order`** (`source_dir`, `kinds`, `limit`): topological sort — **port leaves first**, dependents after.
3. **Per symbol, in order:**
   - Pull source with `tokensave_body` / `tokensave_node`.
   - Map dependencies with `tokensave_callees`; map incoming use with `tokensave_callers`.
   - Confirm the contract with `tokensave_signature`.
   - Apply the ported code with the editing tools (`tokensave_replace_symbol` / `tokensave_str_replace` / `tokensave_insert_at_symbol`) or your normal edit tools.
4. **After each batch:** re-run `tokensave_port_status` to update progress; run `tokensave_diagnostics` to typecheck the target.
5. **Cross-branch parity (if porting across refs):** `tokensave_branch_diff` / `tokensave_changelog`.

## Guardrails

- Never port a symbol before its dependencies (respect `tokensave_port_order`).
- `tokensave_port_status` / `tokensave_port_order` and the lookups are read-only; the editing tools and `tokensave_diagnostics` mutate the working tree / run the toolchain. Only use them when the user wants edits applied, and respect Cursor approval/run-mode.
- `tokensave_diagnostics` forces target dir `.tokensave/target/`; the first run can take minutes.

## Output

- Updated port status (done / remaining) and the per-batch typecheck result.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
