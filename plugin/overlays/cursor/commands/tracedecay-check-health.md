---
description: Check code health for the repo or a directory, including worst offenders and a prioritized fix list.
---

# /tracedecay-check-health

Apply the `tracedecay:code-health` skill.

- **Scope:** the whole repo, or the directory named in `$ARGUMENTS` if one was given.
- Follow that skill's read-only workflow and guardrails; lead with `tracedecay_health` and drill only into weak dimensions. Don't restate the tool ladder here.

Output: the composite health score + weak dimensions, the worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list.
