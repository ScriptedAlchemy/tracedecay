---
name: cleaning-up-dead-code
description: Use when removing or consolidating dead code, unused imports, duplicate implementations, stale helpers, or cleanup findings after confirming callers and references.
---

# Cleaning up dead code

## Workflow

1. **Unreachable symbols → `tokensave_dead_code`** (`include_public: true` for workspace-internal audits; `main`/`test*` are always excluded).
2. **Dead imports → `tokensave_unused_imports`.**
3. **Duplication → `tokensave_redundancy`** (`min_lines`, `similarity_threshold`; buckets: definite / likely / naming_only). For duplicate *discovery* and the pre-write "does a helper already exist" probe, use `tokensave:finding-duplicate-logic`; this skill owns the removal.
4. **Focused pass on a subset → `tokensave_simplify_scan`** (`files`).
5. **Before deleting anything → confirm zero real callers** with `tokensave_callers` / `tokensave_rename_preview`. Be conservative with `pub` items (they may be used outside the indexed scope).
6. **Apply edits** with the `tokensave:atomic-code-edits` primitives (`tokensave_str_replace` / `tokensave_replace_symbol` / `tokensave_multi_str_replace`) or your normal edit tools.
7. **Verify → `tokensave_diagnostics`**, then the `tokensave:running-impacted-tests` skill.

## Measuring (optional)

- Bracket the session via `tokensave:tracking-session-health` to show the before/after health delta.

## Guardrails

- Discovery tools are read-only. The editing tools, `tokensave_diagnostics`, and `session_start`/`session_end` mutate state or run checks; use them when cleanup/verification is relevant and respect Cursor approval/run-mode. Never delete a symbol whose callers/references are non-empty.

## Output

- Removed/consolidated items and the before/after health or test result.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
