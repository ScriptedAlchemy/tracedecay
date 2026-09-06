---
name: tracing-functions
description: 'Who calls X, what X calls, or the path between two symbols. Covers resolving names to exact symbols, trait and dynamic dispatch that text search misses, bounded depth, and reporting coverage gaps instead of "no callers". Locating a symbol is exploring-code.'
---

# Tracing functions

Resolve ambiguous names to exact symbols before following edges. Caller/callee
queries answer relationships; a text occurrence is not a call edge. Begin with
one direction and bounded depth, widening only when the question needs a longer
chain. Batch independent known nodes when the operation supports it.

Inspect trait-dispatch attribution and implementation bodies when the call passes
through an interface. Indexed resolution is not universal runtime coverage:
macros, dynamic dispatch, string-keyed calls, and public users outside the index
may need source or runtime evidence. Report the coverage gap instead of claiming
an empty graph proves no callers.

Call chains connect specified endpoints; rename preview provides broader
reference evidence for a proposed rename without applying it. Hand actual
mutations to `editing-safely`, and test/blast-radius questions to
`assessing-impact`. Keep returned node identities for scoped follow-up.
