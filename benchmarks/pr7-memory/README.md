# PR7 memory, fact, anchor, and migration evidence

> **Historical evidence only.** Preserve the migration fixtures and provenance
> in this directory. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; exact commands, test names/counts,
> snapshots, receipts, attestations, PR packets, and gate fields below are not
> rebuild instructions. Validate current migration and memory behavior directly.

Direct behavioral coverage for the PR7 memory/fact/provenance slice. There is
no measurement harness, owner receipt, gate manifest, content-addressed
acceptance snapshot, signature, trust root, or attestation in this directory.

## Retained provenance

This README is the only historical provenance retained in this directory.
The former workload, evidence index, and provisional result belonged to a
removed harness and were not accepted evidence. Product behavior is validated
directly through the cargo tests below.

## Direct behavioral tests

1. `cargo test --lib application::evidence_assembly::tests`
2. `cargo test --lib sessions::claude_observation_benchmark::tests`
3. `cargo test --test storage_suite migration_test::memory_v2_v19_v23`
4. `cargo test --lib db::retrieval_anchor_authority::tests`
5. `cargo test --lib privacy::tests`
6. `cargo test --test host_event_fixture_test`
7. `cargo test -p tracedecay-domain --test git_topology_anchor_contract`
