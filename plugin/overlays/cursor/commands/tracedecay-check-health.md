---
name: tracedecay-check-health
description: Check code health for the repo or a directory, including worst offenders and a prioritized fix list.
---

# /tracedecay-check-health

Use `tracedecay:code-health`.

- **Scope:** the whole repo, or the directory named in `$ARGUMENTS` if one was given.
- Read-only: lead with `tracedecay_health` and drill only into weak dimensions.

Output: the composite health score + weak dimensions, the worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list.
