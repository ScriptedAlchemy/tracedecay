---
name: tokensave-audit-safety
description: Audit the repo or a directory for ship-blocking risk — panic sites, risk markers, dead code, and untested high-risk symbols.
disable-model-invocation: true
---

# /tokensave-audit-safety

Apply the `tokensave:auditing-code-safety` skill.

- **Scope:** the whole repo, or the directory named after the command if one was given.
- Follow that skill's read-only workflow and guardrails; report findings, don't fix them here.

Output: findings grouped Critical / Warning / Note with file + enclosing symbol, and a prioritized follow-up list.
