---
description: Audit the repo or a directory for ship-blocking risk, panic sites, risk markers, dead code, and untested high-risk symbols.
---

# /tracedecay-audit-safety

Apply the `tracedecay:reviewing-changes` skill.

- **Scope:** the whole repo, or the directory named in `$ARGUMENTS` if one was given.
- Follow that skill's read-only workflow and guardrails; report findings, don't fix them here.

Output: findings grouped Critical / Warning / Note with file + enclosing symbol, and a prioritized follow-up list.
