# PR7 memory, fact, anchor, and migration evidence

> **Historical evidence only.** Preserve the migration fixtures and provenance
> in this directory. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; exact commands, test names/counts,
> snapshots, receipts, attestations, PR packets, and gate fields below are not
> rebuild instructions. Validate current migration and memory behavior directly.

Direct behavioral coverage for the PR7 memory/fact/provenance slice. There is
no measurement harness, owner receipt, gate manifest, content-addressed
acceptance snapshot, signature, trust root, or attestation in this directory.

## Artifacts

| Path | Role |
|---|---|
| [evidence-index.json](evidence-index.json) | Legacy status pointer (`pending`; deprecated `current_acceptance` remains null) |
| [result-provisional.json](result-provisional.json) | Historical local timings only — not accepted evidence |

## Status: pending

Product behavior is accepted through the cargo tests below. Performance
numbers in `result-provisional.json` are diagnostic leftovers from a removed
harness and must not be quoted as accepted PR7 evidence. The retired
`workload-v1.json` recorded that harness's execution shape; its removal does
not upgrade the retained result or evidence index into accepted provenance.

## Direct behavioral tests

1. `cargo test --lib application::evidence_assembly::tests`
2. `cargo test --lib sessions::claude_observation_benchmark::tests`
3. `cargo test --test storage_suite migration_test::memory_v2_v19_v23`
4. `cargo test --lib db::retrieval_anchor_authority::tests`
5. `cargo test --lib privacy::tests`
6. `cargo test --test host_event_fixture_test`
7. `cargo test -p tracedecay-domain --test git_topology_anchor_contract`
