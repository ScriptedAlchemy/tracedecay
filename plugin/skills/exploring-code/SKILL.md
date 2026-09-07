---
name: exploring-code
description: 'Locate a symbol, find where behavior lives, or explain how a flow works across files in an indexed repo. Covers which evidence answers which question (text, symbol, or concept), coverage and freshness caveats, and anchored continuation. Reading a file you already know needs no workflow.'
---

# Exploring code

Choose the evidence the question needs. Literal text search finds strings and
configuration keys; symbol lookup finds declarations; concept search helps when
the implementation's name is unknown. Text matches do not establish call edges.
Known identifiers make useful lexical anchors; preserve them when continuing a
search instead of broadening an already specific question.

Resolve an ambiguous symbol to its file and node before reading its body or
following relationships. An outline helps locate a body in an unfamiliar file;
a direct bounded read is sufficient when the path and relevant region are known.

Read response freshness and coverage with the result. Missing or partial indexed
coverage cannot prove absence. Generation-bound code navigation belongs to the
exact project and worktree snapshot; do not substitute the active graph for an
unavailable requested scope. Continuations retain the original lexical anchors,
preferences, and opaque cursor; a typed mismatch means resolve the query again.

For type/trait questions, see [types and traits](references/types-and-traits.md).
For cross-branch reads, see [other branches](references/other-branches.md).
Call relationships belong to `tracing-functions`; changed consumers and tests
belong to `assessing-impact`.
