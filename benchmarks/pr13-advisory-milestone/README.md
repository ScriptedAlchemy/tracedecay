# PR13 advisory direct tests

The legacy `workload-v1.json` fixture, its Draft-07 `schema-v1.json`, and the
`validate_packet.py` linter were retired: they were never an acceptance packet,
gate manifest, CI receipt, or pass snapshot. The application four-pillar test
owns canonical advisory behavior; the root runtime test reads authentic
GitHub/CI/proximity captures and exercises production typed boundaries without
a composite fixture loader.

Direct product completion requires the named Rust tests and normal CI.
Runtime JUnit remains a CI artifact; nothing here aggregates test output or
derives acceptance state from checked-in packet fields.

Pagination/CAS is a product journey covered by a named Rust test; no owner
receipt or PR-specific evidence artifact is created.

## Open coverage debt: proximity overlap and tiering

`pr13_advisory_proximity_pillar` proves only that the proximity pillar runs
inside the production advisory cycle. Nothing currently asserts that overlap
detection and tiering produce the right answer.

`proximity_file_overlap_and_tiering` used to assert exactly that end to end:
two host-admitted agent sessions editing one file yield a single
`ProximityWarningClassV1::SameFile` evidence row with `overlap_size == 2`, and
`Pr13ProximityRuntimeOwnerV1::evaluate` returns one included
`ProximityTierV1::Immediate` contribution. `992934e03` narrowed
`production_proximity_evidence_authority_v1` from `pub` to `pub(crate)` and
dropped it from the `advisory` re-export list without touching the test, which
left `tests/advisory_runtime_acceptance.rs` unable to compile;
`9e3ca9fd2` then deleted the test rather than repairing the import, and left
this gate pointing at the deleted name. `production_proximity_evidence_authority_v1`
now has no test caller anywhere.

Restoring the assertion needs a decision that is not this packet's to make:
either re-export the authority so an integration test can construct it again,
or move the end-to-end case into an in-crate test beside
`src/application/advisory/proximity_runtime/authority.rs`. Until then this
gate's name must not imply overlap coverage. Plan 37 (PR11–PR13 tiered
proximity) owns the behavior.
