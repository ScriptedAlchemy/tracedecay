---
design_status: current
evidence_class: concept_synthetic
---

# Umbrella Delivery graph

## User job

Understand how multiple repository-specific PRs combine into one product outcome and decide which branch of the outcome needs review next.

## Product behavior

- The umbrella outcome is the selected root; every PR remains separately drillable.
- Rails encode repository ownership, temporal order, status, review coverage, risk, and blocking relationships.
- Selecting a PR preserves the umbrella breadcrumb and opens that PR's journey.
- The view distinguishes required, optional, inferred, blocked, superseded, and shipped relationships.

## Truth boundary

An umbrella Delivery is a correlation projection, not a provider object. Inferred grouping is labeled and reversible; exact Git/PR/CI evidence remains independently inspectable.
