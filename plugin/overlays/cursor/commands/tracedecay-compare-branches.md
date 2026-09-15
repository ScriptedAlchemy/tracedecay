---
name: tracedecay-compare-branches
description: Compare or search another git branch's code graph without switching your checkout.
---

# /tracedecay-compare-branches

Use `tracedecay:exploring-code`.

Interpret `$ARGUMENTS` as one target branch or `<base> <head>`; if absent, list
tracked branches and ask what to compare or search. This is read-only. If a
target is untracked, give the user the `tracedecay branch add <branch>` command.
Return the requested semantic result and surface any branch-fallback warning.
