# Diagnostic flood authority evidence

This directory records three executions of the prebuilt
`diagnostic_publication_stress` integration authority compiled after
`c61f5dce7`. Each execution drives 10,000 distinct generations under
backpressure and asserts one emitted publication, queue depth bounded to one,
and queued bytes bounded by `MAX_PUBLICATION_BYTES`.

These are bounded correctness/rate samples, not a production CLI latency
baseline. N=3 is below p95 and p99 eligibility, so both remain unavailable.
