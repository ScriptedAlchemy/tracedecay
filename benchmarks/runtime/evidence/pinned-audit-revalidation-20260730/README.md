# Pinned-audit runtime revalidation

> **Dated runtime evidence — not acceptance authority.** Preserve these raw
> samples and their provenance, but do not recreate their exact counts,
> snapshots, receipts, attestations, binary/worktree choreography, or gates as
> build prerequisites. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; validate current runtime behavior directly.

This directory records one same-input ABBA revalidation of the now-wired
runtime capture path. The baseline is the explicitly supplied installed
prebuilt binary and the treatment is the explicitly supplied worktree prebuilt
binary. Both run only against disposable profiles owned by the harness.

There are two raw samples per variant. The p50 is eligible under the frozen
two-sample minimum; p95 and p99 remain pending at 40 and 100 matching samples.
This evidence is descriptive and must not be promoted to an SLO gate.
