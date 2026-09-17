---
description: Draft commit, pull-request, or changelog text from semantic changes.
---

# Draft commit

Interpret `$ARGUMENTS` as `pr`, `changelog`, a base ref, or `staged`. If absent,
draft a commit message for current changes. Use commit context for commit text,
PR context for a ref-to-ref description, or changelog evidence for release
notes, adding branch comparison only when it resolves uncertainty.

Return the requested draft. Do not commit, push, create a PR, or publish notes
unless the user separately asks for that action.
