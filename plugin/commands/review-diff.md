---
description: Review the current PR or diff for impact, risk, and quality via the TraceDecay code graph.
---

# Review diff

Review the working-tree diff, or the base ref or PR named in `$ARGUMENTS`.
Follow the bundled `reviewing-changes` skill. Read the actual diff, add semantic
context for its changed symbols and dependents, and deepen only where a concrete
risk warrants more impact, quality, safety, or test evidence.

This command is read-only and does not run tests. Report only actionable defects
with locations, failure modes, and evidence, plus material unresolved coverage
and the relevant verification. Use `test-changes` when execution is requested.
