---
description: Compare or search Git refs through the project-wide code graph without switching the checkout.
argument-hint: "[ref | base head]"
---

# Compare Git refs

Interpret `$ARGUMENTS` as one target ref versus the current worktree, or
`<base> <head>` for an exact comparison.

1. Resolve refs and available indexed generations through the daemon's Git/code
   application operations.
2. Search an exact ref snapshot with `tracedecay_branch_search`.
3. Compare exact snapshots with `tracedecay_branch_diff`.
4. Preserve commit, worktree, generation, freshness, and coverage provenance.

All graph generations live in the canonical project Grafeo store. Do not create
or request a branch database, run `branch add`, silently serve an ancestor, or
switch the user's checkout. If the requested snapshot is absent or indexing,
return that typed state and the explicit indexing/refresh operation the user
may choose.

Output the hits or added/removed/changed symbols with their exact snapshot
provenance. Report any `tracedecay_metrics:` observation.
