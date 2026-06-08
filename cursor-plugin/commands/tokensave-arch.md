---
name: tokensave-arch
description: Generate a high-level architecture overview of the repo or a directory.
---

# /tokensave-arch

Apply the `tokensave:architecture-overview` skill.

- **Scope:** the whole repo, or the directory in `$ARGUMENTS` if provided.
- Follow that skill's read-only workflow and guardrails; don't restate the tool ladder here.

Output: a layered module map, dependency hotspots/violations, and a prioritized risk list.
