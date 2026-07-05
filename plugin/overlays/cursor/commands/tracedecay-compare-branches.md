---
name: tracedecay-compare-branches
description: Compare or search another git branch's code graph without switching your checkout.
---

# /tracedecay-compare-branches

Use `tracedecay:exploring-code`.

- **Args:** interpret `$ARGUMENTS` as a single target branch, or "<base> <head>" to diff two branches; if absent, start with `tracedecay_branch_list` and ask what to search/compare.
- Read-only. If a target branch isn't tracked, tell the user to run `tracedecay branch add <branch>` first.

Output: the cross-branch search hits or the added/removed/changed symbol lists, with any branch-fallback warning surfaced.
