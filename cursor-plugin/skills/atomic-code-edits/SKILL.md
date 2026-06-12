---
name: atomic-code-edits
description: Use when editing source with safe anchored primitives: unique string replacement, atomic multi-replace, anchored insert, whole-symbol rewrite, structural ast-grep rewrite, or mechanical edits that should re-index the graph.
---

# Atomic code edits

These tools mutate files; invoke them only when an edit is relevant to the task and respect Cursor approval/run-mode. Each writer targets a single file and triggers an in-place re-index, so the graph stays fresh.

## Pick the primitive

1. **Unique string swap → `tokensave_str_replace`** (`path`, `old_str`, `new_str`): fails unless `old_str` matches exactly once — the safest default; use instead of sed/awk.
2. **Several swaps, all-or-nothing → `tokensave_multi_str_replace`** (`path`, `replacements` as `[[old, new], …]`): every pair must match exactly once or the whole edit aborts.
3. **Insert at an anchor → `tokensave_insert_at`** (`path`, `anchor` = unique string or 1-indexed line number, `content`, `before?`): add a line/block before or after a unique anchor.
4. **Insert around a symbol → `tokensave_insert_at_symbol`** (`symbol`, `content`, `position`: `before`|`after`): drop code adjacent to a named symbol's range (prefer a qualified name).
5. **Rewrite a whole symbol → `tokensave_replace_symbol`** (`symbol`, `new_source`): replace a function/method/struct/enum; `new_source` must include the declaration line. Refused on unresolved ambiguity.
6. **Structural pattern rewrite → `tokensave_ast_grep_rewrite`** (`path`, `pattern`, `rewrite`, ast-grep SGPattern syntax): rewrite every match of a syntactic pattern in one file — e.g. swap argument order or wrap calls (`foo($A)` → `bar(foo($A))`) — where string matching would over- or under-match. Repeat per file for multi-file rewrites.

## Before & after

- **Preview blast radius first → `tokensave_rename_preview`** (`node_id`) for renames, or resolve the target with the `tokensave:searching-for-code` ladder so you edit the right symbol. For multi-site mechanical refactors (rename everywhere, signature/field changes), run the `tokensave:refactoring-safely` recon first and use its checklist as the edit plan.
- **Verify after editing:** typecheck via `tokensave:fixing-build-and-type-errors`, then `tokensave:running-impacted-tests`.

## Guardrails

- `str_replace` / `multi_str_replace` / `insert_at` are single-file; `insert_at_symbol` / `replace_symbol` resolve by (qualified) name and refuse ambiguous matches — disambiguate rather than forcing.
- `tokensave_ast_grep_rewrite` shells out to the external `ast-grep` binary: it is only registered when `ast-grep` is on PATH and fails when it is not installed. If the tool is absent, tell the user to install ast-grep rather than approximating the rewrite with regex.

## Output

- The files/symbols changed and the verification result.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
