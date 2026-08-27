---
design_status: current
evidence_class: concept_synthetic
---

# Global PR inbox

## User job

Find every ingested pull request across all projects, identify urgent or weakly reviewed work, and recognize PRs that jointly produce one umbrella outcome.

## Product behavior

- Default Delivery entry with no project selected.
- Filters by project, status, risk, coverage, agent, and unresolved evidence.
- The PR list remains primary and accessible; the graph groups correlated PRs without hiding individual entries.
- Selecting a PR opens its journey. Selecting an umbrella opens the multi-repository outcome graph.

## Truth boundary

The plate is synthetic. Production correlation must label exact, inferred, ambiguous, stale, missing, and unavailable links separately. Provider controls are read-only unless a real authorized write path exists.
