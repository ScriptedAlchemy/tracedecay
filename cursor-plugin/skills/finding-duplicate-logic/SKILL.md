---
name: finding-duplicate-logic
description: Use before writing a new helper or utility to check whether equivalent code already exists, or when finding duplicate logic, similar symbols, repeated implementations, and consolidation candidates.
---

# Finding duplicate logic

Two uses: a 30-second pre-write probe before adding any new helper, and a deeper duplication audit.

## Pre-write probe (always cheap)

1. **By name/keyword → `tracedecay_search`** with the helper's likely names; **fuzzy twins → `tracedecay_similar`** (`parse_cfg` vs `config_parse`).
2. **By shape → `tracedecay_signature_search`** (return type / param substring / async): "any fn taking `&Path` and returning `Result<Config>`?"
3. **By concept → `tracedecay_context`** (one call, `task` = what the helper should do) when naming guesses fail.
4. **Found one?** Inspect with `tracedecay_body` and reuse or extend it instead of writing a new copy.

## Duplication audit

5. **Functional duplicates → `tracedecay_redundancy`** (`min_lines?`, `similarity_threshold?`, `path?`, `max_pairs?`): AST-isomorphism / control-flow / call-sequence / token-shingle matching, bucketed `definite` / `likely` / `naming_only`. Trust `definite`; verify `likely` by reading both bodies; treat `naming_only` as a hint only.
6. **Consolidating?** Keep the better-tested copy (check `tracedecay_test_map` on both), then hand removal to `tracedecay:cleaning-up-dead-code` and the edits to `tracedecay:atomic-code-edits`.

## Guardrails

- All discovery here is read-only and parallel-safe. `tracedecay_redundancy` is computed lazily and cached — the first call on a fresh index can be slow on large repos; keep `path` / `max_pairs` tight.
- This skill finds and judges duplication; deleting or merging belongs to the cleanup and edit skills.

## Output

- The existing helper to reuse (or its confirmed absence), or the bucketed duplicate pairs with a consolidation recommendation.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
