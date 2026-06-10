---
name: tokensave-audit
description: Sweep the repo or a directory for panic sites, risk markers, dead code, and untested high-risk symbols via the tokensave code graph.
disable-model-invocation: true
---

# /tokensave-audit

Apply the `tokensave:auditing-code-safety` skill.

- **Scope:** the whole repo, or the directory named after the command if one was given.
- Follow that skill's read-only workflow and guardrails; report findings, don't fix them here.

Output: findings grouped Critical / Warning / Note with file + enclosing symbol, and a prioritized follow-up list.
