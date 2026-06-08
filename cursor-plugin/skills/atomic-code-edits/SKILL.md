---
name: atomic-code-edits
description: Apply safe, anchored source edits with tokensave's atomic edit primitives (unique-match string replace, atomic multi-replace, anchored insert, symbol-level rewrite) that auto re-index after writing. Use to apply precise/anchored edits or rewrite a whole symbol without regex/shell-quoting hazards.
---

# Atomic code edits

These tools mutate files; invoke them only when an edit is relevant to the task and respect Cursor approval/run-mode. Each writer targets a single file and triggers an in-place re-index, so the graph stays fresh.

## Pick the primitive

1. **Unique string swap → `tokensave_str_replace`** (`path`, `old_str`, `new_str`): fails unless `old_str` matches exactly once — the safest default; use instead of sed/awk.
2. **Several swaps, all-or-nothing → `tokensave_multi_str_replace`** (`path`, `replacements` as `[[old, new], …]`): every pair must match exactly once or the whole edit aborts.
3. **Insert at an anchor → `tokensave_insert_at`** (`path`, `anchor` = unique string or 1-indexed line number, `content`, `before?`): add a line/block before or after a unique anchor.
4. **Insert around a symbol → `tokensave_insert_at_symbol`** (`symbol`, `content`, `position`: `before`|`after`): drop code adjacent to a named symbol's range (prefer a qualified name).
5. **Rewrite a whole symbol → `tokensave_replace_symbol`** (`symbol`, `new_source`): replace a function/method/struct/enum; `new_source` must include the declaration line. Refused on unresolved ambiguity.

## Before & after

- **Preview blast radius first → `tokensave_rename_preview`** (`node_id`) for renames, or resolve the target with the `tokensave:searching-for-code` ladder so you edit the right symbol.
- **Verify after editing:** typecheck via `tokensave:fixing-build-and-type-errors`, then `tokensave:running-impacted-tests`.

## Guardrails

- `str_replace` / `multi_str_replace` / `insert_at` are single-file; `insert_at_symbol` / `replace_symbol` resolve by (qualified) name and refuse ambiguous matches — disambiguate rather than forcing.
- For structural multi-file rewrites the graph can't anchor, fall back to your normal edit tools.

## Output

- The files/symbols changed and the verification result.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
