---
name: tracedecay-check-health
description: 'Use to check code health for the repo or a directory, including worst offenders and a prioritized fix list.'
---

# Check health

Use for repo or directory code-health checks, worst offenders, and prioritized fix lists.

Use `tracedecay:code-health`.

- **Scope:** the whole repo, or a specific directory if one is named.
- Read-only: lead with `tracedecay_health` and drill only into weak dimensions.

Output: the composite health score + weak dimensions, the worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list.
