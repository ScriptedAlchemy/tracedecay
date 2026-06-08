---
name: tokensave-health
description: Produce a code-health scorecard for the repo or a directory via the tokensave code graph.
---

# /tokensave-health

Apply the `tokensave:code-health-report` skill.

- **Scope:** the whole repo, or the directory in `$ARGUMENTS` if provided.
- Follow that skill's read-only workflow and guardrails; lead with `tokensave_health` and drill only into weak dimensions. Don't restate the tool ladder here.

Output: the composite health score + weak dimensions, the worst offenders (complexity, duplication, god files, doc gaps, panic sites, test-risk), and a prioritized fix list.
