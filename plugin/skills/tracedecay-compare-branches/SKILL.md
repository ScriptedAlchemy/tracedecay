---
name: tracedecay-compare-branches
description: 'Use to compare or search another git branch''s code graph without switching your checkout.'
---

# Compare branches

Use when asked to compare or search another git branch's code graph without switching your checkout.

Route this through the `tracedecay:exploring-code` skill, using the cross-branch tools (`tracedecay_branch_list`, `tracedecay_branch_diff`, `tracedecay_branch_search`).

- **Target:** a single branch to compare against the current branch, or "<base> <head>" to diff two branches. If none is given, start with `tracedecay_branch_list` and ask what to search or compare.
- Follow that skill's read-only workflow. If a target branch isn't tracked, tell the user to run `tracedecay branch add <branch>` in the terminal first, and surface any branch-fallback warning.

Output: the cross-branch search hits or the added/removed/changed symbol lists.
