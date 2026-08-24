# Diagnostic flood authority evidence

> **Dated runtime evidence — not acceptance authority.** Preserve these raw
> samples and their provenance, but do not recreate their exact counts,
> snapshots, receipts, attestations, or gate choreography as build
> prerequisites. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; validate current runtime behavior directly.

This directory records three executions of the prebuilt
`diagnostic_publication_stress` integration authority compiled after
`c61f5dce7`. Each execution drives 10,000 distinct generations under
backpressure and asserts one emitted publication, queue depth bounded to one,
and queued bytes bounded by `MAX_PUBLICATION_BYTES`.

These are bounded correctness/rate samples, not a production CLI latency
baseline. N=3 is below p95 and p99 eligibility, so both remain unavailable.
