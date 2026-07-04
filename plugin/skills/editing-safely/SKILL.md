---
name: editing-safely
description: 'Use before writing a new function or helper (it may already exist), and before any rename, signature, or field change — the graph lists every call site.'
---

# Editing safely

Three phases: probe for existing code before writing, recon every affected
site before the first edit, then apply with anchored primitives that re-index
the graph in place.

## Pre-write duplicate probe (always cheap, run before any new helper)

1. **By name/keyword → `tracedecay_search`**; **fuzzy twins →
   `tracedecay_similar`** (`parse_cfg` vs `config_parse`).
2. **By shape → `tracedecay_signature_search`** (return type / param
   substring / async): "any fn taking `&Path` and returning `Result<Config>`?"
3. **By concept → `tracedecay_context`** (one call, `task` = what the helper
   should do) when naming guesses fail.
4. **Deeper audit → `tracedecay_redundancy`** (`min_lines?`,
   `similarity_threshold?`, `path?`, `max_pairs?`): buckets `definite` /
   `likely` / `naming_only`. Trust `definite`; verify `likely`; treat
   `naming_only` as a hint. Lazily computed and cached — keep `path` /
   `max_pairs` tight on large repos.
5. **Found one?** Inspect with `tracedecay_body` and reuse or extend it. When
   consolidating, keep the better-tested copy (check `tracedecay_test_map` on
   both).

## Refactor recon (read-only, before the first edit)

1. **Resolve the target → node ID** (`tracedecay:exploring-code` ladder).
2. **Recon by refactor type:**
   - Rename / move → `tracedecay_rename_preview` (`node_id`): every edge
     where it appears as source or target (preview only — nothing renames).
   - Signature change → `tracedecay_callers` (every call site must adapt)
     plus `tracedecay_signature_search` for shape-twins.
   - Field rename/remove/new invariant → `tracedecay_field_sites`
     (`Struct::field`): write sites are the blast radius.
   - Newly required field → `tracedecay_constructors`: every struct-literal
     site with missing-field lists.
   - Post-rename name collisions → `tracedecay_similar`.
3. **Risk check → `tracedecay_impact`** (shallow `max_depth` first) when the
   target is widely depended on. The recon output is the edit checklist.

Run this read-only recon in one shot for a symbol or `Struct::field` with
[scripts/safe-edit-sequence.sh](scripts/safe-edit-sequence.sh).

## Apply with anchored primitives

1. **Unique string swap → `tracedecay_str_replace`** (`path`, `old_str`,
   `new_str`): fails unless `old_str` matches exactly once — the safest
   default; use instead of sed/awk.
2. **Several swaps, all-or-nothing → `tracedecay_multi_str_replace`**
   (`path`, `replacements` as `[[old, new], …]`): every pair must match
   exactly once or the whole edit aborts.
3. **Insert at an anchor → `tracedecay_insert_at`** (`path`, `anchor` =
   unique string or 1-indexed line, `content`, `before?`); **around a symbol
   → `tracedecay_insert_at_symbol`** (`symbol`, `content`, `position`).
4. **Rewrite a whole symbol → `tracedecay_replace_symbol`** (`symbol`,
   `new_source` including the declaration line). Refused on unresolved
   ambiguity — disambiguate rather than forcing.
5. **Structural pattern rewrite → `tracedecay_ast_grep_rewrite`** (`path`,
   `pattern`, `rewrite`, ast-grep SGPattern syntax): rewrite every match of a
   syntactic pattern in one file (e.g. `foo($A)` → `bar(foo($A))`); repeat
   per file for multi-file rewrites.

## Porting code

1. **Baseline → `tracedecay_port_status`** (`source_dir`, `target_dir`,
   `kinds`); **order → `tracedecay_port_order`**: topological sort — port
   leaves first, dependents after; never port a symbol before its
   dependencies.
2. Per symbol: pull source with `tracedecay_body`, map dependencies with
   `tracedecay_callees` / `tracedecay_callers`, confirm the contract with
   `tracedecay_signature`, apply with the primitives above.
3. After each batch: re-run `tracedecay_port_status`; typecheck with
   `tracedecay_diagnostics`. Cross-branch parity → `tracedecay_branch_diff` /
   `tracedecay_changelog`.

## Guardrails

- Probe and recon steps are read-only; the edit primitives mutate files and
  trigger an in-place re-index — invoke them only when an edit is relevant
  and respect Cursor approval/run-mode.
- Recon sees only the indexed scope: `pub` items may have external users, and
  macro-generated or string-keyed references won't appear in the graph — grep
  once for the bare name before declaring the checklist complete.
- `tracedecay_ast_grep_rewrite` shells out to the external `ast-grep` binary
  and is only registered when it is on PATH; if absent, tell the user to
  install ast-grep rather than approximating the rewrite with regex.
- **Verify after editing:** typecheck via
  `tracedecay:fixing-build-and-type-errors`, then run the affected tests via
  `tracedecay:assessing-impact`.

## If tools are deferred or MCP fails

- Deferred (names listed without schemas): load once with ToolSearch —
  `select:tracedecay_search,tracedecay_similar,tracedecay_rename_preview,tracedecay_str_replace,tracedecay_replace_symbol`
  (one batched call, add others needed) — then call normally.
- MCP error/timeout/disconnect: same tool, same args, via shell:
  `tracedecay tool <name> --key value` (see `tracedecay:using-the-cli`). Never
  query `.tracedecay` databases directly; never abandon the graph over transport.

## Deliverable

Do not end this workflow without: the recon checklist (sites grouped by file),
the files/symbols changed, the helper reused (or its confirmed absence), and
the verification result. Report any `tracedecay_metrics:` line to the user.
