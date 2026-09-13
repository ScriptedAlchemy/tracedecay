---
description: Run or inspect agent-managed memory curation and its terminal run record.
argument-hint: "[subject]"
---

# Curate memory

Interpret `$ARGUMENTS` as a fact, entity, query, curation scope, or existing
run. Follow the bundled `project-memory` skill and load its curation reference
only for a broad curation request or run inspection. Resolve the registered
project before mutation. A read-only inspection must not launch another run,
and the dashboard opens only when the user asks for visual curation.

Broad curation uses `tracedecay_fact_store_curate`; the daemon owns its task,
validation, and supported effects. Preserve the returned run id and inspect
terminal state and only advertised artifacts through the automation run views.
If a failed run records applied operations, report them and the required
reconciliation before considering a retry.

Direct fact operations are for exact administration requests and are independent
of curator runs. Prefer supersession when a newer fact corrects an older one.
Removal is permanent; an exact deletion instruction is sufficient, while an
ambiguous target must be resolved before removal. Verify the final state through
canonical fact reads and report the run or fact identities that establish it.
