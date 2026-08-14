---
name: reviewing-changes
description: 'Use when reviewing a PR, branch, or working-tree diff, auditing ship risk (panic/unsafe/todo, dead code, untested hotspots), or drafting commit/PR/changelog text. Trigger before "gh pr diff" — pr_context/diff_context compute changed symbols offline. Do NOT use for pure test selection (tracedecay:assessing-impact).'
---

# Reviewing changes

```
NO REVIEW BY READING RAW DIFFS. Semantic change context comes first:
diff_context for the working tree, pr_context for ref-to-ref — both offline.
```

Announce: "Using tracedecay:reviewing-changes for <diff/PR>."

## Diff review

1. Changed files: working tree as-is, or `git diff --name-only <base>...HEAD`.
2. Semantic summary — one call:
   - Working tree / file list → `tracedecay_diff_context` (`files`).
   - PR / ref-to-ref → `tracedecay_pr_context` (`base_ref`, `head_ref`).
   Both return modified symbols + dependents + affected tests from the local
   graph. Use `gh` only for review comments/CI status — never for the diff
   analysis itself.
3. Deepen only where needed: `tracedecay_impact` on a high-risk changed
   symbol; `tracedecay_affected` only if step 2's test set is insufficient.
4. Quality scan of just the changed files → `tracedecay_simplify_scan`.
5. Duplicate-body risk or repeated helper logic in touched code →
   `tracedecay_redundancy` scoped to the changed directory/file set. Treat
   `definite` as consolidation evidence; verify `likely` — especially pairs
   whose `overlap_kind` is `body_vector` (vector-only signal); `naming_only`
   appears only with `include_naming_only: true` and needs another signal.
6. Risk: `tracedecay_test_risk` on changed paths; `tracedecay_unsafe_patterns`
   on changed files (`exclude_tests: true` for production-only — unwrap/panic
   in tests is normal, and an `unsafe { }` block is an attention site, not
   automatically a finding).
7. Git index preview/apply stays on the application surfaces
   `tracedecay_git_preview` then `tracedecay_git_apply` — never invent a
   parallel git write path.
8. For generation-bound native Git evidence, use `tracedecay_git_status`,
   `tracedecay_git_diff`, `tracedecay_git_history`, `tracedecay_git_blame`,
   and `tracedecay_git_hunks`. These reads never authorize a ref or index
   mutation.
9. Branch feedback cycle (CI localization, GitHub review comments, proximity):
   `tracedecay_feedback_advisory_cycle`. It returns the canonical diagnostics
   result and daemon-minted read handle. Read-only; never post, update,
   resolve, or reply on GitHub.

## Safety audit (ship-readiness) and dead-code cleanup

Read [references/safety-audit.md](references/safety-audit.md) for the full
sweep (`tracedecay_unsafe_patterns` → `tracedecay_todos` →
`tracedecay_dead_code`/`tracedecay_unused_imports`/`tracedecay_unmounted_files`
→ `tracedecay_test_risk` → ranking) and the delete-safely protocol (zero-caller confirmation with
`tracedecay_callers` before any deletion; conservative on `pub`).

## Drafting commit & PR text

| Deliverable | Call |
|---|---|
| Commit message | `tracedecay_commit_context` (`staged_only`) — changed symbols + recent commit style |
| PR description | `tracedecay_pr_context` → Summary / Impact / Tests |
| Release notes | `tracedecay_changelog` (`from_ref`, `to_ref`); sanity-check `tracedecay_branch_diff` |

Drafts text only — `git commit` / `gh pr create` stay with the user.

## Rules

- Review and audit are read-only. Edits → `tracedecay:editing-safely`;
  verification → `tracedecay:assessing-impact`.
- Large diffs: scoped read-only subagents per file group or risk category with
  cited findings; the parent owns severity and dedup.
- Truncated with a `handle`? Narrow by file/symbol first; `tracedecay_retrieve`
  only when the omitted risk detail is needed.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_diff_context,tracedecay_pr_context,tracedecay_simplify_scan,tracedecay_redundancy,tracedecay_unsafe_patterns,tracedecay_test_risk`.
- MCP error: `tracedecay tool pr_context --base-ref main --head-ref HEAD` etc.
  (see `tracedecay:using-the-cli`). gh being unauthenticated or offline is NOT
  a blocker — pr_context/diff_context never touch the network.

## Deliverable

Do not end without findings grouped Critical / Warning / Note, each with file
+ enclosing symbol, plus the impacted areas and test set — or the drafted
commit/PR/changelog text when that was the ask. An empty review must state
what was scanned (tools + scope), not just "looks good". Report any
`tracedecay_metrics:` line.
