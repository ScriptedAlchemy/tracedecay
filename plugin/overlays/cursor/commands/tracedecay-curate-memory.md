---
name: tracedecay-curate-memory
description: Curate or inspect TraceDecay memory facts and agent-managed curation runs.
---

# /tracedecay-curate-memory

Use `tracedecay:project-memory`.

Interpret `$ARGUMENTS` as a fact, query, curation scope, or existing run. Resolve
the registered project before mutation. Inspect an existing run without launching
another; open the dashboard only when the user wants visual curation.

Broad curation uses `fact_store_curate` and the `project-memory` curation
reference. Direct operations require an exact fact-administration request.
Prefer supersession for corrections. Exact deletion instructions are sufficient;
resolve ambiguous targets before permanent removal. Inspect returned terminal
state and advertised artifacts, report committed effects before any retry, and
verify final facts with canonical reads.
