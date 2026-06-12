---
name: tracedecay-compare-branches
description: Compare or search another git branch's code graph without switching your checkout.
disable-model-invocation: true
---

# /tracedecay-compare-branches

Apply the `tracedecay:cross-branch-investigation` skill.

- **Args:** interpret the text after the command as either a single target branch to compare against the current branch, or "<base> <head>" to diff two branches; if absent, start with `tracedecay_branch_list` and ask what to search/compare.
- Follow that skill's read-only workflow; if a target branch isn't tracked, tell the user to run `tracedecay branch add <branch>` in the terminal first.

Output: the cross-branch search hits or the added/removed/changed symbol lists, with any branch-fallback warning surfaced.
