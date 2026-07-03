---
name: tracedecay-check-health
description: 'Use to check code health for the repo or a directory, including worst offenders and a prioritized fix list.'
---

# Check health

Use when asked to check code health for the repo or a directory, including worst offenders and a prioritized fix list.

Route this through the `tracedecay:code-health` skill.

- **Scope:** the whole repo, or a specific directory if one is named.
- Follow that skill's read-only workflow and guardrails: lead with `tracedecay_health` and drill only into weak dimensions.

Output: the composite health score + weak dimensions, the worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list.
