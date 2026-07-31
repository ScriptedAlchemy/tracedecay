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
| [workload-v1.json](workload-v1.json) | Versioned phase/workload pin (historical measurement shape) |
| [evidence-index.json](evidence-index.json) | Legacy status pointer (`pending`; deprecated `current_acceptance` remains null) |
| [result-provisional.json](result-provisional.json) | Historical local timings only — not accepted evidence |

## Status: pending

Product behavior is accepted through the cargo tests below. Performance
numbers in `result-provisional.json` are diagnostic leftovers from a removed
harness and must not be quoted as accepted PR7 evidence.

## Direct behavioral tests

1. `cargo test --lib application::evidence_assembly::tests::authorized_drilldown_expands_contribution_span_set_and_exact_members -- --exact`
2. `cargo test --lib sessions::claude_observation_benchmark::tests::every_provider_executes_a_production_path_and_exact_no_op -- --exact`
3. `cargo test --test storage_suite migration_test::memory_v2_v19_v23::test_migrate_v19_pr7_schema_preserves_data_and_enforces_v20_to_v22_contracts -- --exact`
4. `cargo test --lib db::retrieval_anchor_authority::tests::disposition_replay_survives_restart_without_resurrection -- --exact`
5. `cargo test --lib privacy::tests::provider_neutral_workflow_fact_redaction_leaks_no_raw_secret -- --exact`
6. `cargo test --test host_event_fixture_test canonical_and_linked_worktree_events_share_retained_project_authority -- --exact`
7. `cargo test -p tracedecay-domain --test git_topology_anchor_contract`
