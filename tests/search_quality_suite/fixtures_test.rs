//! Fixture integrity and contamination-partition tests (no indexing, no
//! retrieval): the committed fixtures must load, validate, and digest-match
//! their own bytes, and the development/sealed-holdout partitions must be
//! structurally disjoint.

use std::collections::BTreeSet;

use crate::evaluation::{EvalPartitionV1, FixtureAuthorityV1};
use crate::fixtures;
use crate::holdout::{self, HoldoutSealStatus};

#[test]
fn manifest_loads_and_validates() {
    let manifest = fixtures::load_manifest();
    assert_eq!(manifest.corpus.len(), 10, "corpus snapshot count");
    let partitions: BTreeSet<_> = manifest
        .partitions
        .iter()
        .map(|spec| (spec.partition, spec.query_count))
        .collect();
    assert_eq!(
        partitions,
        BTreeSet::from([
            (EvalPartitionV1::Development, 22),
            (EvalPartitionV1::SealedHoldout, 9),
        ]),
        "frozen partition spec"
    );
}

#[test]
fn corpus_and_artifact_digests_match_committed_bytes() {
    let manifest = fixtures::load_manifest();
    fixtures::verify_corpus_digests(&manifest);
    fixtures::verify_artifact_digests(&manifest);
}

#[test]
fn workload_loads_with_stable_digest_and_declared_groups() {
    let manifest = fixtures::load_manifest();
    let first = fixtures::load_workload();
    let second = fixtures::load_workload();
    assert_eq!(
        first.digest, second.digest,
        "workload digest is deterministic across loads"
    );
    assert_eq!(first.development_queries().count(), 22);
    assert_eq!(first.sealed_holdout_queries().count(), 9);
    fixtures::validate_workload_against_manifest(&first, &manifest)
        .expect("workload declares only manifest groups and corpus documents");
}

#[test]
fn partitions_are_disjoint_and_match_the_manifest_spec() {
    let manifest = fixtures::load_manifest();
    let workload = fixtures::load_workload();
    let development: BTreeSet<_> = workload
        .development_queries()
        .map(|query| &query.query_id)
        .collect();
    let holdout: BTreeSet<_> = workload
        .sealed_holdout_queries()
        .map(|query| &query.query_id)
        .collect();
    assert!(
        development.is_disjoint(&holdout),
        "development and sealed-holdout query ids must be disjoint"
    );
    for spec in &manifest.partitions {
        let actual = workload
            .queries
            .iter()
            .filter(|query| query.partition == spec.partition)
            .count() as u32;
        assert_eq!(
            actual,
            spec.query_count,
            "partition {} query count drifted from the manifest",
            spec.partition.as_str()
        );
    }
}

#[test]
fn development_labels_cross_validate_against_workload() {
    let manifest = fixtures::load_manifest();
    let workload = fixtures::load_workload();
    let first = fixtures::load_development_labels();
    let second = fixtures::load_development_labels();
    assert_eq!(
        first.digest, second.digest,
        "label digest is deterministic across loads"
    );
    fixtures::validate_labels_against_workload(&first, &workload, &manifest)
        .expect("development labels reference only development queries and corpus documents");
}

#[test]
fn holdout_seal_is_committed_and_verified_without_reading_labels() {
    let manifest = fixtures::load_manifest();
    let seal = fixtures::load_holdout_seal(&manifest);
    match holdout::seal_status(&seal) {
        HoldoutSealStatus::AuthorizedStoreOnly => {
            // Development validation checks metadata only. It never opens,
            // hashes, or parses the authorized-store label payload.
        }
        HoldoutSealStatus::InvalidMetadata => {
            panic!("sealed holdout locator metadata is invalid");
        }
    }
}

#[test]
fn plan15_fixture_bundle_cross_validates_real_checkpoint_artifacts() {
    let bundle = fixtures::load_fixture_bundle();
    bundle
        .validate()
        .expect("all Plan 15 fixture artifacts cross-validate");
    fixtures::verify_fixture_bundle_digests(&bundle);

    assert_eq!(
        bundle.manifest.authority,
        FixtureAuthorityV1::ContractOnly,
        "this packet must never claim locked acceptance authority"
    );
    assert_eq!(
        bundle.snapshots[0].repository_commit, "eda50f53000ab4f96ef30e1f3a46b748b3fea6e0",
        "the sanitized corpus is pinned to an immutable repository checkpoint"
    );
    assert_eq!(
        bundle.development_labels.partition,
        EvalPartitionV1::Development
    );
    assert_eq!(
        bundle.run.scope,
        crate::evaluation::EvalRunScopeV1::Development
    );
    assert_eq!(bundle.run.authority, FixtureAuthorityV1::ContractOnly);
    for task in &bundle.tasks {
        task.verify_prompt_digest()
            .expect("sanitized task prompt digest verifies");
    }
    assert!(
        bundle
            .evidence_index
            .entries
            .iter()
            .all(|entry| !entry.acceptance_authority),
        "fixture integrity claims are not acceptance evidence"
    );
}

#[test]
fn contamination_groups_cannot_cross_partition_boundaries() {
    let mut bundle = fixtures::load_fixture_bundle();
    let development_query = bundle
        .workload
        .development_queries()
        .next()
        .expect("development query")
        .query_id
        .clone();
    let sealed_group = bundle
        .contamination_partitions
        .groups
        .iter_mut()
        .find(|group| group.partition == EvalPartitionV1::SealedHoldout)
        .expect("sealed contamination group");
    sealed_group.query_ids.push(development_query);

    assert!(matches!(
        bundle.validate(),
        Err(crate::evaluation::EvaluationContractError::PartitionViolation(_))
    ));
}

#[test]
fn run_and_evidence_index_digests_are_tamper_evident() {
    let bundle = fixtures::load_fixture_bundle();
    bundle.run.verify_digest().expect("run digest verifies");
    bundle
        .evidence_index
        .verify_digest()
        .expect("evidence-index digest verifies");

    let mut run = bundle.run;
    run.output_schema.push_str("-tampered");
    assert!(matches!(
        run.verify_digest(),
        Err(crate::evaluation::EvaluationContractError::DigestMismatch { .. })
    ));

    let mut index = bundle.evidence_index;
    index.entries[0].claim.push_str("-tampered");
    assert!(matches!(
        index.verify_digest(),
        Err(crate::evaluation::EvaluationContractError::DigestMismatch { .. })
    ));
}

#[test]
fn bundle_rejects_unknown_temporal_supersession_and_run_artifact_drift() {
    let mut bad_event = fixtures::load_fixture_bundle();
    bad_event.temporal_events[1].supersedes_event_id =
        Some(crate::evaluation::TemporalEventId::new("event-missing").unwrap());
    assert!(matches!(
        bad_event.validate(),
        Err(crate::evaluation::EvaluationContractError::CoverageViolation(_))
    ));

    let mut bad_run = fixtures::load_fixture_bundle();
    bad_run.run.artifact_files.clear();
    bad_run.run.digest = bad_run.run.compute_digest().unwrap();
    assert!(matches!(
        bad_run.validate(),
        Err(crate::evaluation::EvaluationContractError::CoverageViolation(_))
            | Err(crate::evaluation::EvaluationContractError::Empty { .. })
    ));
}

#[test]
fn bundle_rejects_out_of_bounds_spans_and_observable_canaries() {
    let mut bad_span = fixtures::load_fixture_bundle();
    bad_span.context_spans[0].byte_end = u64::MAX;
    assert!(matches!(
        bad_span.validate(),
        Err(crate::evaluation::EvaluationContractError::CoverageViolation(_))
    ));

    let mut bad_canary = fixtures::load_fixture_bundle();
    bad_canary.workload.queries[20]
        .allowed_scope_ids
        .push("scope-session-private".to_string());
    bad_canary.workload.digest = bad_canary.workload.compute_digest().unwrap();
    assert!(matches!(
        bad_canary.validate(),
        Err(crate::evaluation::EvaluationContractError::CoverageViolation(_))
            | Err(crate::evaluation::EvaluationContractError::DigestMismatch { .. })
    ));
}
