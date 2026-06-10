---
name: tokensave-arch
description: Generate a high-level architecture overview of the repo or a directory.
disable-model-invocation: true
---

# /tokensave-arch

Apply the `tokensave:architecture-overview` skill.

- **Scope:** the whole repo, or the directory named after the command if one was given.
- Follow that skill's read-only workflow and guardrails; don't restate the tool ladder here.

Output: a layered module map, dependency hotspots/violations, and a prioritized risk list.
