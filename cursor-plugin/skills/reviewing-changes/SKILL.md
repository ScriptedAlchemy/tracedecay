---
name: reviewing-changes
description: 'Use when reviewing a PR, branch, or working-tree diff, auditing ship-blocking risk (panic/unsafe/todo sites, dead code, untested hotspots), cleaning up dead or duplicate code, or drafting commit messages, PR descriptions, and changelogs from semantic diff context.'
---

# Reviewing changes

## Diff review

1. **Get changed files** — working tree, or `git diff --name-only
   <base>...HEAD` (default base `main`).
2. **Semantic change summary:** working tree / file list →
   `tracedecay_diff_context` (`files`): modified symbols + dependents +
   affected tests; ref-to-ref PR → `tracedecay_pr_context` (`base_ref`,
   `head_ref`).
3. **Go deeper only if needed:** `tracedecay_impact` (`node_id`) to widen the
   blast radius on a high-risk changed symbol; `tracedecay_affected`
   (`files`) only when step 2's test set is not enough.
4. **Quality scan of just the changed files → `tracedecay_simplify_scan`**
   (`files`): duplications, dead code, coupling, complexity hotspots.
5. **Risk surfacing:** `tracedecay_test_risk` on changed paths;
   `tracedecay_unsafe_patterns` on changed files.

## Safety audit (ship-readiness sweep)

1. **Panic & unsafe sites → `tracedecay_unsafe_patterns`** (`kinds?` to
   narrow to `unwrap`/`unsafe`, `exclude_tests: true` for production-only,
   `path?`): each hit carries file, line, kind, enclosing symbol, `in_test`.
2. **Unfinished work → `tracedecay_todos`** (`kinds:
   ["FIXME","HACK","XXX","UNIMPLEMENTED"]`).
3. **Unreachable code → `tracedecay_dead_code`** (`include_public: true` for
   workspace-internal audits) and **`tracedecay_unused_imports`**.
4. **Risky and untested → `tracedecay_test_risk`**: high-complexity,
   high-fan-in symbols with weak coverage.
5. **Rank:** production panic/unsafe in hot paths first (cross-check fan-in
   with `tracedecay_callers`), then UNIMPLEMENTED/HACK markers, then untested
   high-risk symbols, then dead code and imports.

## Dead-code cleanup

1. Discover with `tracedecay_dead_code` / `tracedecay_unused_imports` /
   `tracedecay_redundancy`; focused pass → `tracedecay_simplify_scan` (`files`).
2. **Before deleting anything → confirm zero real callers** with
   `tracedecay_callers` / `tracedecay_rename_preview`. Be conservative with
   `pub` items (they may be used outside the indexed scope). Never delete a
   symbol whose callers/references are non-empty.
3. Apply edits via `tracedecay:editing-safely`; verify with
   `tracedecay_diagnostics` and the affected tests
   (`tracedecay:assessing-impact`). Optionally bracket the cleanup with the
   session-health delta in `tracedecay:code-health`.

## Drafting commit & PR text

1. **Commit message → `tracedecay_commit_context`** (`staged_only`): changed
   symbols + file roles + recent commit style.
2. **PR description → `tracedecay_pr_context`** (`base_ref`, `head_ref`):
   Summary / Impact / Tests.
3. **Release notes → `tracedecay_changelog`** (`from_ref`, `to_ref`);
   sanity-check with `tracedecay_branch_diff`.
4. Drafts text only — leave `git commit` / `gh pr create` to the user or a
   dedicated git workflow.

## Guardrails

- Review and audit are read-only; do not edit or run tests from those flows —
  hand edits to `tracedecay:editing-safely` and verification to
  `tracedecay:assessing-impact`.
- `unwrap`/`panic!` inside tests is normal — respect `exclude_tests` /
  `in_test` before flagging. An `unsafe { }` block is a review-attention
  site, not automatically a finding to "fix".
- For large diffs, use scoped read-only subagents by file group or risk
  category; require cited findings — the parent agent owns severity,
  deduplication, and the final call.
- If diff context is truncated with a `handle`, narrow by file/symbol first;
  call `tracedecay_retrieve` only when the omitted risk detail is needed.

## Output

- Findings grouped **Critical / Warning / Note** with file + enclosing
  symbol, the impacted areas and test set, removed/consolidated items, or the
  drafted commit/PR/changelog text. Pairs with the `pr-review-canvas` plugin
  if installed.
- If any result includes a `tracedecay_metrics:` line, report the savings to
  the user.
