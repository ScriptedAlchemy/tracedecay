---
name: tracedecay-check-health
description: Check code health — a scorecard for the repo or a directory with the worst offenders and a prioritized fix list.
disable-model-invocation: true
---

# /tracedecay-check-health

Apply the `tracedecay:code-health-report` skill.

- **Scope:** the whole repo, or the directory named after the command if one was given.
- Follow that skill's read-only workflow and guardrails; lead with `tracedecay_health` and drill only into weak dimensions. Don't restate the tool ladder here.

Output: the composite health score + weak dimensions, the worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list.
