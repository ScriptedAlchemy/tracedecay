---
name: finding-duplicate-logic
description: Check whether code already exists before writing it, and find functionally duplicated implementations. Use before writing any new helper/util ("is there already a function that does this?"), for "find duplicate code", "have we implemented X twice?", or planning a consolidation.
---

# Finding duplicate logic

Two uses: a 30-second pre-write probe before adding any new helper, and a deeper duplication audit.

## Pre-write probe (always cheap)

1. **By name/keyword → `tokensave_search`** with the helper's likely names; **fuzzy twins → `tokensave_similar`** (`parse_cfg` vs `config_parse`).
2. **By shape → `tokensave_signature_search`** (return type / param substring / async): "any fn taking `&Path` and returning `Result<Config>`?"
3. **By concept → `tokensave_context`** (one call, `task` = what the helper should do) when naming guesses fail.
4. **Found one?** Inspect with `tokensave_body` and reuse or extend it instead of writing a new copy.

## Duplication audit

5. **Functional duplicates → `tokensave_redundancy`** (`min_lines?`, `similarity_threshold?`, `path?`, `max_pairs?`): AST-isomorphism / control-flow / call-sequence / token-shingle matching, bucketed `definite` / `likely` / `naming_only`. Trust `definite`; verify `likely` by reading both bodies; treat `naming_only` as a hint only.
6. **Consolidating?** Keep the better-tested copy (check `tokensave_test_map` on both), then hand removal to `tokensave:cleaning-up-dead-code` and the edits to `tokensave:atomic-code-edits`.

## Guardrails

- All discovery here is read-only and parallel-safe. `tokensave_redundancy` is computed lazily and cached — the first call on a fresh index can be slow on large repos; keep `path` / `max_pairs` tight.
- This skill finds and judges duplication; deleting or merging belongs to the cleanup and edit skills.

## Output

- The existing helper to reuse (or its confirmed absence), or the bucketed duplicate pairs with a consolidation recommendation.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
