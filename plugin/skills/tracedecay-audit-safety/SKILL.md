---
name: tracedecay-audit-safety
description: 'Use to audit the repo or a directory for ship-blocking risk, panic sites, risk markers, dead code, and untested high-risk symbols.'
---

# Audit safety

Use for repo or directory audits covering ship-blocking risk, panic sites, risk markers, dead code, or untested high-risk symbols.

Use `tracedecay:reviewing-changes`.

- **Scope:** the whole repo, or a specific directory if one is named.
- Read-only: report findings, do not fix them here.

Output: findings grouped Critical / Warning / Note with file + enclosing symbol, and a prioritized follow-up list.
