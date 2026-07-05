---
name: tracedecay-audit-safety
description: Audit the repo or a directory for ship-blocking risk, panic sites, risk markers, dead code, and untested high-risk symbols.
---

# /tracedecay-audit-safety

Use `tracedecay:reviewing-changes`.

- **Scope:** the whole repo, or the directory named in `$ARGUMENTS` if one was given.
- Read-only: report findings, don't fix them here.

Output: findings grouped Critical / Warning / Note with file + enclosing symbol, and a prioritized follow-up list.
