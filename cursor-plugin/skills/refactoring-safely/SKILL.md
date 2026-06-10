---
name: refactoring-safely
description: Recon and execute mechanical refactors — renames, signature changes, field add/remove/rename, moving helpers — with every affected site enumerated before editing. Use for "rename X everywhere", "change this function's signature", "add/remove a struct field", or any refactor where a missed call site breaks the build.
---

# Refactoring safely

Enumerate every affected site *before* the first edit; the recon output is the edit checklist.

## Workflow

1. **Resolve the target → node ID** with `tokensave_search` / `tokensave_find_exact_symbol` (resolver ladder: `tokensave:searching-for-code`).
2. **Recon, by refactor type (read-only):**
   - Rename / move a symbol → `tokensave_rename_preview` (`node_id`): every edge where it appears as source or target.
   - Signature change → `tokensave_callers` (every call site must adapt) plus `tokensave_signature_search` for shape-twins that should change together (e.g. every `fn` returning `Result<_, OldError>`).
   - Field rename/remove/new invariant → `tokensave_field_sites` (`Struct::field`): write sites are the blast radius.
   - Newly required field → `tokensave_constructors`: every struct-literal site, with missing-field lists.
   - Names that will collide or confuse after the rename → `tokensave_similar`.
3. **Risk check → `tokensave_impact`** (`node_id`, shallow `max_depth` first) when the target is widely depended on; widen only if the picture is incomplete.
4. **Apply via `tokensave:atomic-code-edits`:** `tokensave_multi_str_replace` for the per-file site checklist (all-or-nothing), `tokensave_replace_symbol` for the definition itself, `tokensave_ast_grep_rewrite` for structural call-shape changes (e.g. argument reorder).
5. **Verify:** typecheck via `tokensave:fixing-build-and-type-errors`, then `tokensave:running-impacted-tests`.

## Guardrails

- Steps 1–3 are read-only; step 4 mutates files and step 5 runs toolchains — respect Cursor approval/run-mode. `tokensave_rename_preview` only previews; nothing renames automatically.
- Recon sees only the indexed scope: `pub` items may have external users, and macro-generated or string-keyed references won't appear in the graph — grep once for the bare name before declaring the checklist complete.

## Output

- The recon checklist (sites grouped by file), the edits applied, and the clean verify result.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
