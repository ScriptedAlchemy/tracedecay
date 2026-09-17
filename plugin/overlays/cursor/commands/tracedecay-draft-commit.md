---
name: tracedecay-draft-commit
description: Draft commit, pull-request, or changelog text from semantic changes.
---

# /tracedecay-draft-commit

Use `tracedecay:reviewing-changes`.

Interpret `$ARGUMENTS` as `pr`, `changelog`, a base ref, or `staged`; if absent,
draft a commit message for current changes. Return only the requested draft. Do
not commit, push, create a PR, or publish unless the user separately asks.
