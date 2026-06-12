---
name: atomic-code-edits
description: Apply safe, anchored source edits — unique-match string replace, all-or-nothing multi-replace, anchored insert, whole-symbol rewrite, and structural pattern rewrite — each auto re-indexing the graph after writing. Use when editing code ("replace this function", "insert after X", "rewrite every call site"), for multi-part edits that must land atomically, or instead of sed/awk and regex/shell-quoting hazards.
---

# Atomic code edits

These tools mutate files; invoke them only when an edit is relevant to the task and respect Cursor approval/run-mode. Each writer targets a single file and triggers an in-place re-index, so the graph stays fresh.

## Pick the primitive

1. **Unique string swap → `tracedecay_str_replace`** (`path`, `old_str`, `new_str`): fails unless `old_str` matches exactly once — the safest default; use instead of sed/awk.
2. **Several swaps, all-or-nothing → `tracedecay_multi_str_replace`** (`path`, `replacements` as `[[old, new], …]`): every pair must match exactly once or the whole edit aborts.
3. **Insert at an anchor → `tracedecay_insert_at`** (`path`, `anchor` = unique string or 1-indexed line number, `content`, `before?`): add a line/block before or after a unique anchor.
4. **Insert around a symbol → `tracedecay_insert_at_symbol`** (`symbol`, `content`, `position`: `before`|`after`): drop code adjacent to a named symbol's range (prefer a qualified name).
5. **Rewrite a whole symbol → `tracedecay_replace_symbol`** (`symbol`, `new_source`): replace a function/method/struct/enum; `new_source` must include the declaration line. Refused on unresolved ambiguity.
6. **Structural pattern rewrite → `tracedecay_ast_grep_rewrite`** (`path`, `pattern`, `rewrite`, ast-grep SGPattern syntax): rewrite every match of a syntactic pattern in one file — e.g. swap argument order or wrap calls (`foo($A)` → `bar(foo($A))`) — where string matching would over- or under-match. Repeat per file for multi-file rewrites.

## Before & after

- **Preview blast radius first → `tracedecay_rename_preview`** (`node_id`) for renames, or resolve the target with the `tracedecay:searching-for-code` ladder so you edit the right symbol. For multi-site mechanical refactors (rename everywhere, signature/field changes), run the `tracedecay:refactoring-safely` recon first and use its checklist as the edit plan.
- **Verify after editing:** typecheck via `tracedecay:fixing-build-and-type-errors`, then `tracedecay:running-impacted-tests`.

## Guardrails

- `str_replace` / `multi_str_replace` / `insert_at` are single-file; `insert_at_symbol` / `replace_symbol` resolve by (qualified) name and refuse ambiguous matches — disambiguate rather than forcing.
- `tracedecay_ast_grep_rewrite` shells out to the external `ast-grep` binary: it is only registered when `ast-grep` is on PATH and fails when it is not installed. If the tool is absent, tell the user to install ast-grep rather than approximating the rewrite with regex.

## Output

- The files/symbols changed and the verification result.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
