---
name: tracedecay-audit-safety
description: 'Use to audit the repo or a directory for ship-blocking risk, panic sites, risk markers, dead code, and untested high-risk symbols.'
---

# Audit safety

Use when asked to audit the repo or a directory for ship-blocking risk, panic sites, risk markers, dead code, or untested high-risk symbols.

Route this through the `tracedecay:reviewing-changes` skill.

- **Scope:** the whole repo, or a specific directory if one is named.
- Follow that skill's read-only workflow and guardrails: report findings, do not fix them here.

Output: findings grouped Critical / Warning / Note with file + enclosing symbol, and a prioritized follow-up list.
