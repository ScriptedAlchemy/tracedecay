# Missing-daemon after-shell p95 evidence

> **Dated runtime evidence — not acceptance authority.** Preserve these raw
> samples and their provenance, but do not recreate their exact counts,
> snapshots, receipts, attestations, or gate choreography as build
> prerequisites. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; validate current runtime behavior directly.

This directory records 40 isolated-profile invocations of the committed
`hook-cursor-after-shell` product command against an intentionally absent
daemon socket. The sample count meets the frozen p95 minimum; p99 remains
unavailable below 100 matching samples.

Each raw sample records the process-owning harness wall time, same-binary
startup control, direct product-command wall time, the non-negative
direct-minus-startup residual, lifecycle-wrapper overhead, typed daemon
unavailability, and process-tree cleanup. The residual is not authoritative
internal handler timing.
