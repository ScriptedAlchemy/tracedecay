---
name: tracing-functions
description: Use when the user asks who calls a function, what it calls, or how one function reaches another. Skip when the user wants occurrences of a string rather than call relationships. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Tracing functions

Resolve ambiguous names to exact symbols before following edges. Caller/callee
queries answer relationships; a text occurrence is not a call edge. Begin with
one direction and bounded depth, widening only when the question needs a longer
chain. Batch independent known nodes when the operation supports it.

Inspect trait-dispatch attribution and implementation bodies when the call passes
through an interface. Indexed resolution is not universal runtime coverage:
macros, dynamic dispatch, string-keyed calls, and public users outside the index
may need source or runtime evidence.

Call chains connect specified endpoints; rename preview provides broader
reference evidence for a proposed rename without applying it. Hand actual
mutations to `editing-safely`, and test/blast-radius questions to
`assessing-impact`. Keep returned node identities for scoped follow-up.

**Report:** the callers or callees with their files, the depth searched, and the
coverage gap (macros, dynamic dispatch, users outside the index). Do not call an
empty result proof that nothing calls the symbol.
