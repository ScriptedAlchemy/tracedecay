---
description: Compare or search another git branch's code graph without switching your checkout.
argument-hint: "[branch | base head]"
---

# Compare branches

Interpret `$ARGUMENTS` as one target branch to compare with the current branch,
or as `<base> <head>`. If absent, list tracked branches and ask which comparison
or search the user wants. Follow the bundled `exploring-code` guidance for
cross-branch reads. These operations are read-only and do not switch checkout.

Branch tracking is opt-in per branch. If a target branch isn't tracked, tell the user to run `tracedecay branch add <branch>` in the terminal first. A branch-fallback `WARNING` prefix means results came from the nearest tracked ancestor — surface that to the user.

Return the requested search hits or semantic differences and surface any
branch-fallback warning.
