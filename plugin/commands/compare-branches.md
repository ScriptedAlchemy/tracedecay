---
description: Compare or search another git branch's code graph without switching your checkout.
argument-hint: "[branch | base head]"
---

# Compare branches

Interpret `$ARGUMENTS` as either a single target branch to compare against the current branch, or "<base> <head>" to diff two branches. If absent, start with `tracedecay_branch_list` and ask what to search or compare.

1. What's tracked → `tracedecay_branch_list`.
2. Search another branch → `tracedecay_branch_search` (`branch`, `query`).
3. Compare branches → `tracedecay_branch_diff` (`base`, `head`, optional `file`, `kind`) — added / removed / changed symbols, read-only and never touching your checkout.

Branch tracking is opt-in per branch. If a target branch isn't tracked, tell the user to run `tracedecay branch add <branch>` in the terminal first. A branch-fallback `WARNING` prefix means results came from the nearest tracked ancestor — surface that to the user.

Output: the cross-branch search hits or the added/removed/changed symbol lists, with any branch-fallback warning surfaced. If any result includes a `tracedecay_metrics:` line, report the savings.
