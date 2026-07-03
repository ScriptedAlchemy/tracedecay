---
name: tracedecay-compare-branches
description: 'Use to compare or search another git branch''s code graph without switching your checkout.'
---

# Compare branches

Use to compare or search another git branch's code graph without switching your checkout.

Use `tracedecay:exploring-code` with `tracedecay_branch_list`, `tracedecay_branch_diff`, and `tracedecay_branch_search`.

- **Target:** a single branch to compare against the current branch, or "<base> <head>" to diff two branches. If none is given, start with `tracedecay_branch_list` and ask what to search or compare.
- Read-only. If a target branch isn't tracked, tell the user to run `tracedecay branch add <branch>` first, and surface any branch-fallback warning.

Output: the cross-branch search hits or the added/removed/changed symbol lists.
